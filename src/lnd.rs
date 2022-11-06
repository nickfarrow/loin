use std::convert::TryFrom;
use std::fmt;
use std::num::TryFromIntError;
use std::str::FromStr;
use std::sync::Arc;

use bitcoin::consensus::{Decodable, Encodable};
use bitcoin::psbt::PartiallySignedTransaction;
use bitcoin::{Address, Amount, Transaction};
use ln_types::P2PAddress;
use log::info;
use tokio::sync::Mutex as AsyncMutex;
use tonic_lnd::lnrpc::funding_transition_msg::Trigger;
use tonic_lnd::lnrpc::{
    FundingPsbtVerify, FundingTransitionMsg, OpenChannelRequest, OpenStatusUpdate,
    WalletBalanceRequest,
};
use tonic_lnd::walletrpc::RequiredReserveRequest;

use crate::scheduler::ChannelId;

// #[derive(Debug)]
// pub enum LndError {
//     Generic(tonic_lnd::Error),
//     ConnectError(tonic_lnd::ConnectError),
//     ParseBitcoinAddressFailed(bitcoin::util::address::Error),
//     VersionRequestFailed(tonic_lnd::Error),
//     ParseVersionFailed { version: String, error: std::num::ParseIntError },
//     LNDTooOld(String),
//     BadPsbt(bitcoin::consensus::encode::Error),
//     Publish(String),
// }

// impl fmt::Display for LndError {
//     fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
//         match self {
//             LndError::Generic(error) => error.fmt(f),
//             LndError::ConnectError(error) => error.fmt(f),
//             LndError::ParseBitcoinAddressFailed(err) => err.fmt(f),
//             LndError::VersionRequestFailed(_) => write!(f, "failed to get LND version"),
//             LndError::ParseVersionFailed { version, error: _ } => {
//                 write!(f, "Unparsable LND version '{}'", version)
//             }
//             LndError::BadPsbt(error) => error.fmt(f),
//             LndError::LNDTooOld(version) => write!(
//                 f,
//                 "LND version {} is too old - it would cause GUARANTEED LOSS of sats!",
//                 version
//             ),
//             LndError::Publish(error) => error.fmt(f),
//         }
//     }
// }

// impl std::error::Error for LndError {
//     fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
//         match self {
//             LndError::Generic(error) => Some(error),
//             LndError::ConnectError(error) => Some(error),
//             LndError::ParseBitcoinAddressFailed(error) => Some(error),
//             LndError::VersionRequestFailed(error) => Some(error),
//             LndError::ParseVersionFailed { version: _, error } => Some(error),
//             LndError::BadPsbt(error) => Some(error),
//             LndError::LNDTooOld(_) => None,
//             LndError::Publish(_) => None,
//         }
//     }
// }

// impl From<tonic_lnd::Error> for LndError {
//     fn from(value: tonic_lnd::Error) -> Self { LndError::Generic(value) }
// }

// impl From<tonic_lnd::ConnectError> for LndError {
//     fn from(value: tonic_lnd::ConnectError) -> Self { LndError::ConnectError(value) }
// }

#[derive(Clone)]
pub struct LndClient(Arc<AsyncMutex<tonic_lnd::Client>>);

impl LndClient {
    /// New [LndClient] from [Config].
    pub async fn from_config(config: &crate::config::Config) -> Result<Self, LndError> {
        let raw_client = tonic_lnd::connect(
            config.lnd_address.clone(),
            &config.lnd_cert_path,
            &config.lnd_macaroon_path,
        )
        .await?;

        Self::new(raw_client).await
    }

    pub async fn new(mut client: tonic_lnd::Client) -> Result<Self, LndError> {
        let response = client
            .lightning()
            .get_info(tonic_lnd::lnrpc::GetInfoRequest {})
            .await
            .map_err(LndError::VersionRequestFailed)?;
        let version_str = &response.get_ref().version;
        let version = Self::parse_lnd_version(version_str)?;

        if version < (0, 15, 1) {
            return Err(LndError::LNDTooOld(version_str.clone().to_owned()));
        }

        Ok(Self(Arc::new(AsyncMutex::new(client))))
    }

    fn parse_lnd_version(version_str: &str) -> Result<(u64, u64, u64), LndError> {
        let trim_from = version_str.find('-').unwrap_or(version_str.len());
        let mut iter =
            version_str.get(..trim_from).expect("trim_from should always succeed").split('.');

        let mut parse_next = || {
            iter.next().map(|v| v.parse::<u64>()).transpose().map_err(|e| {
                LndError::ParseVersionFailed { version: version_str.to_string(), error: e }
            })
        };

        let major = parse_next()?.expect("split returned empty iterator");
        let minor = parse_next()?.unwrap_or(0);
        let patch = parse_next()?.unwrap_or(0);

        Ok((major, minor, patch))
    }

    /// Ensures that we are connected to the node of address.
    pub async fn ensure_connected(&self, node: P2PAddress) -> Result<(), LndError> {
        let pubkey = node.node_id.to_string();
        let peer_addr =
            tonic_lnd::lnrpc::LightningAddress { pubkey, host: node.as_host_port().to_string() };
        let connect_req =
            tonic_lnd::lnrpc::ConnectPeerRequest { addr: Some(peer_addr), perm: true, timeout: 60 };

        let mut client = self.0.lock().await;
        match client.lightning().connect_peer(connect_req).await {
            Err(err) if err.message().starts_with("already connected to peer") => Ok(()),
            result => {
                result?;
                Ok(())
            }
        }
    }

    /// Obtains a new bitcoin bech32 address from the our lnd node.
    pub async fn get_new_bech32_address(&self) -> Result<Address, LndError> {
        let mut client = self.0.lock().await;
        let response = client
            .lightning()
            .new_address(tonic_lnd::lnrpc::NewAddressRequest { r#type: 0, account: String::new() })
            .await?;
        response.get_ref().address.parse::<Address>().map_err(LndError::ParseBitcoinAddressFailed)
    }

    pub async fn get_p2p_address(&self) -> Result<P2PAddress, LndError> {
        let mut client = self.0.lock().await;
        let response = client
            .lightning()
            .get_info(tonic_lnd::lnrpc::GetInfoRequest { ..Default::default() })
            .await?;
        let p2p_address = P2PAddress::from_str(&response.into_inner().uris[0])
            .map_err(LndError::ParseP2PAddressFailed)?;
        Ok(p2p_address)
    }

    /// Requests to open a channel with remote node, returning the psbt of the funding transaction.
    pub async fn open_channel(
        &self,
        req: OpenChannelRequest,
    ) -> Result<Option<PartiallySignedTransaction>, LndError> {
        let client = &mut *self.0.lock().await;
        let mut response = client.lightning().open_channel(req).await?;
        let stream = response.get_mut();

        while let Some(OpenStatusUpdate { pending_chan_id, update: Some(update) }) =
            stream.message().await?
        {
            use tonic_lnd::lnrpc::open_status_update::Update;
            match update {
                Update::PsbtFund(ready) => {
                    let psbt = PartiallySignedTransaction::consensus_decode(&mut &*ready.psbt)
                        .map_err(LndError::Decode)?;
                    info!(
                        "PSBT received from LND for pending chan id {:?}: {:#?}",
                        pending_chan_id, psbt
                    );
                    assert_eq!(psbt.unsigned_tx.output.len(), 1);

                    return Ok(Some(psbt));
                }
                x => return Err(LndError::UnexpectedUpdate(x)),
            }
        }
        Ok(None)
    }

    /// Sends the `FundingPsbtVerify` message to remote lnd nodes to finalize channels of given
    /// channel ids.
    pub async fn verify_funding<I>(&self, funded_psbt: &[u8], chan_ids: I) -> Result<(), LndError>
    where
        I: IntoIterator<Item = ChannelId>,
    {
        let handles = chan_ids
            .into_iter()
            .map(|chan_id| {
                let client = self.clone();
                let req = FundingTransitionMsg {
                    trigger: Some(Trigger::PsbtVerify(FundingPsbtVerify {
                        pending_chan_id: chan_id.into(),
                        funded_psbt: funded_psbt.to_vec(),
                        skip_finalize: true,
                    })),
                };
                tokio::spawn(async move { client.funding_state_step(req).await })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle.await.unwrap()?;
        }

        Ok(())
    }

    pub async fn funding_state_step(&self, req: FundingTransitionMsg) -> Result<(), LndError> {
        let client = &mut *self.0.lock().await;
        client.lightning().funding_state_step(req).await?;
        Ok(())
    }

    pub async fn fund_original_psbt(
        &self,
        address: &bitcoin::Address,
        amount: bitcoin::Amount,
    ) -> Result<PartiallySignedTransaction, LndError> {
        let client = &mut *self.0.lock().await;
        let client = client.wallet();

        let mut outputs = std::collections::HashMap::with_capacity(1);
        outputs.insert(address.to_string(), amount.as_sat()); // todo double check u64 as_sat?
        let tx_template = tonic_lnd::walletrpc::TxTemplate { outputs, ..Default::default() };
        let template = Some(tonic_lnd::walletrpc::fund_psbt_request::Template::Raw(tx_template));
        let fund_psbt = tonic_lnd::walletrpc::FundPsbtRequest { template, ..Default::default() };

        let response = client.fund_psbt(fund_psbt).await?;
        // it is the caller's responsibility to either spend the locked UTXOs (by finalizing and then publishing the transaction)
        //or to unlock/release the locked UTXOs in case of an error on the caller's side.
        let stream = response.get_ref();
        let raw_psbt = stream.funded_psbt.as_slice();
        let funded_psbt =
            PartiallySignedTransaction::consensus_decode(raw_psbt).map_err(LndError::BadPsbt)?;

        Ok(funded_psbt)
    }

    pub async fn sign_psbt(
        &self,
        funded_psbt: PartiallySignedTransaction,
    ) -> Result<PartiallySignedTransaction, LndError> {
        let client = &mut *self.0.lock().await;
        let client = client.wallet();

        let mut encoder = base64::write::EncoderWriter::new(Vec::new(), base64::STANDARD);
        funded_psbt.consensus_encode(&mut encoder).unwrap();
        let funded_psbt = encoder.finish().unwrap();
        // it is the caller's responsibility to either spend the locked UTXOs (by finalizing and then publishing the transaction)
        //or to unlock/release the locked UTXOs in case of an error on the caller's side.
        let req = tonic_lnd::walletrpc::SignPsbtRequest { funded_psbt, ..Default::default() };
        let res = client.sign_psbt(req).await?;
        // TODO !IMPORTANT! else if error
        // unlock / release locked utxos
        let stream = res.get_ref();
        let signed_psbt = stream.signed_psbt.as_slice();

        let tx =
            PartiallySignedTransaction::consensus_decode(signed_psbt).map_err(LndError::BadPsbt)?;
        Ok(tx)
    }

    pub async fn finalize_psbt(
        &self,
        funded_psbt: PartiallySignedTransaction,
    ) -> Result<Transaction, LndError> {
        let client = &mut *self.0.lock().await;
        let client = client.wallet();

        let mut encoder = base64::write::EncoderWriter::new(Vec::new(), base64::STANDARD);
        funded_psbt.consensus_encode(&mut encoder).unwrap();
        let funded_psbt = encoder.finish().unwrap();
        let req = tonic_lnd::walletrpc::FinalizePsbtRequest { funded_psbt, ..Default::default() };
        let res = client.finalize_psbt(req).await?;
        // TODO !IMPORTANT! else if error
        // unlock / release locked utxos
        let stream = res.get_ref();
        let raw_final_tx = stream.raw_final_tx.as_slice();

        let tx = Transaction::consensus_decode(raw_final_tx).map_err(LndError::BadPsbt)?;
        Ok(tx)
    }

    pub async fn broadcast(&self, tx: Transaction) -> Result<bitcoin::Txid, LndError> {
        let client = &mut *self.0.lock().await;
        let client = client.wallet();

        let mut encoder = base64::write::EncoderWriter::new(Vec::new(), base64::STANDARD);
        tx.consensus_encode(&mut encoder).unwrap();
        let tx_hex = encoder.finish().unwrap();
        let req = tonic_lnd::walletrpc::Transaction { tx_hex, ..Default::default() };
        let res = client.publish_transaction(req).await?;
        let stream = res.get_ref();
        if &stream.publish_error != "" {
            return Err(LndError::Publish(stream.publish_error.to_owned()));
        }
        Ok(tx.txid())
    }

    pub async fn required_reserve(
        &self,
        additional_public_channels: u32,
    ) -> Result<Amount, LndError> {
        let client = &mut *self.0.lock().await;
        let res = client
            .wallet()
            .required_reserve(RequiredReserveRequest { additional_public_channels })
            .await?;
        let amount =
            u64::try_from(res.get_ref().required_reserve).map_err(LndError::ParseAsSatFailed)?;
        Ok(Amount::from_sat(amount))
    }

    pub async fn wallet_balance(&self) -> Result<Amount, LndError> {
        let client = &mut *self.0.lock().await;
        let res = client.lightning().wallet_balance(WalletBalanceRequest {}).await?;
        let amount =
            u64::try_from(res.get_ref().total_balance).map_err(LndError::ParseAsSatFailed)?;
        Ok(Amount::from_sat(amount))
    }
}

#[derive(Debug)]
pub enum LndError {
    Generic(tonic_lnd::Error),
    ConnectError(tonic_lnd::ConnectError),
    Decode(bitcoin::consensus::encode::Error),
    ParseBitcoinAddressFailed(bitcoin::util::address::Error),
    ParseAsSatFailed(TryFromIntError),
    ParseP2PAddressFailed(ln_types::p2p_address::ParseError),
    VersionRequestFailed(tonic_lnd::Error),
    UnexpectedUpdate(tonic_lnd::lnrpc::open_status_update::Update),
    ParseVersionFailed { version: String, error: std::num::ParseIntError },
    LNDTooOld(String),
    BadPsbt(bitcoin::consensus::encode::Error),
    Publish(String),
}

impl fmt::Display for LndError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            LndError::Generic(e) => e.fmt(f),
            LndError::ConnectError(e) => e.fmt(f),
            LndError::Decode(e) => e.fmt(f),
            LndError::ParseBitcoinAddressFailed(e) => e.fmt(f),
            LndError::ParseAsSatFailed(err) => err.fmt(f),
            LndError::ParseP2PAddressFailed(e) => e.fmt(f),
            LndError::VersionRequestFailed(_) => write!(f, "failed to get LND version"),
            LndError::UnexpectedUpdate(e) => write!(f, "Unexpected channel update {:?}", e),
            LndError::ParseVersionFailed { version, error: _ } => {
                write!(f, "Unparsable LND version '{}'", version)
            }
            LndError::LNDTooOld(version) => write!(
                f,
                "LND version {} is too old - it would cause GUARANTEED LOSS of sats!",
                version
            ),
            LndError::BadPsbt(error) => error.fmt(f),
            LndError::Publish(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for LndError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LndError::Generic(e) => Some(e),
            LndError::ConnectError(e) => Some(e),
            LndError::Decode(e) => Some(e),
            LndError::ParseBitcoinAddressFailed(e) => Some(e),
            LndError::ParseAsSatFailed(_) => None,
            LndError::ParseP2PAddressFailed(e) => Some(e),
            LndError::VersionRequestFailed(e) => Some(e),
            Self::UnexpectedUpdate(_) => None,
            LndError::ParseVersionFailed { version: _, error } => Some(error),
            LndError::LNDTooOld(_) => None,
            LndError::BadPsbt(error) => Some(error),
            LndError::Publish(_) => None,
        }
    }
}

impl From<tonic_lnd::Error> for LndError {
    fn from(value: tonic_lnd::Error) -> Self { LndError::Generic(value) }
}

impl From<tonic_lnd::ConnectError> for LndError {
    fn from(value: tonic_lnd::ConnectError) -> Self { LndError::ConnectError(value) }
}
