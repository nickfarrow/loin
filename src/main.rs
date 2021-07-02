use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use bitcoin::util::address::Address;
use bitcoin::util::psbt::PartiallySignedTransaction;
use std::convert::TryInto;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 7 {
        println!("{:?}", args);
        println!("args: <bind_port> <address> <cert> <macaroon> <dest_node_uri> <amount>");
        return Ok(());
    }
    let mut client = tonic_lnd::connect(args[2].clone(), &args[3], &args[4])
        .await
        .expect("failed to connect");
    let address = client.new_address(tonic_lnd::rpc::NewAddressRequest { r#type: 0, account: String::new(), }).await?.into_inner().address;
    let amount = args[6].parse().expect("invalid amount");
    println!("bitcoin:{}?amount={}.{:08}&pj=https://example.com/pj", address, amount / 100000000, amount % 100000000);

    let address = address.parse::<Address>().expect("lnd returned invalid address");

    let addr = ([127, 0, 0, 1], args[1].parse().expect("invalid port number")).into();
    let mut node_pubkey = [0; 33];
    hex::decode_to_slice(&args[5], &mut node_pubkey).expect("invalid node pubkey");

    let service = make_service_fn(move |_| {
        let address = address.clone();
        let client = client.clone();

        async move {

            Ok::<_, hyper::Error>(service_fn(move |request| handle_web_req(address.clone(), client.clone(), request, node_pubkey, amount)))
        }
    });

    let server = Server::bind(&addr).serve(service);

    println!("Listening on http://{}", addr);

    server.await?;

    Ok(())
}

async fn handle_web_req(address: Address, mut lnd: tonic_lnd::Client, req: Request<Body>, node_pubkey: [u8; 33], amount: u64) -> Result<Response<Body>, hyper::Error> {
    use bitcoin::consensus::{Decodable, Encodable};

    match (req.method(), req.uri().path()) {
        (&Method::GET, "/") => Ok(Response::new(Body::from(
            "Check terminal output for payjoin link and use it in one of the supported wallets",
        ))),

        (&Method::POST, "/pj") => {
            let base64_bytes = hyper::body::to_bytes(req.into_body()).await?;
            let bytes = base64::decode(&base64_bytes).unwrap();
            let mut reader = &*bytes;
            let our_script = address.script_pubkey();
            let mut psbt = PartiallySignedTransaction::consensus_decode(&mut reader).unwrap();
            psbt.global.unsigned_tx.output.retain(|output| output.script_pubkey != our_script);
            // TODO: verify segwit!
            for input in &mut psbt.global.unsigned_tx.input {
                // clear signature
                input.script_sig = bitcoin::blockdata::script::Script::new();
            }

            let chid = rand::random::<[u8; 32]>();
            let psbt_shim = tonic_lnd::rpc::PsbtShim {
                pending_chan_id: Vec::from(&chid as &[_]),
                base_psbt: Vec::new(),
                no_publish: true,
            };

            let funding_shim = tonic_lnd::rpc::funding_shim::Shim::PsbtShim(psbt_shim);
            let funding_shim = tonic_lnd::rpc::FundingShim {
                shim: Some(funding_shim),
            };

            let open_channel = tonic_lnd::rpc::OpenChannelRequest {
                node_pubkey: Vec::from(&node_pubkey as &[_]),
                local_funding_amount: amount.try_into().expect("amount too large"),
                push_sat: 0,
                private: false,
                min_htlc_msat: 0,
                remote_csv_delay: 0,
                spend_unconfirmed: false,
                close_address: String::new(),
                funding_shim: Some(funding_shim),
                remote_max_value_in_flight_msat: amount,
                remote_max_htlcs: 10,
                max_local_csv: 288,
                ..Default::default()
            };
            let mut update_stream = lnd.open_channel(open_channel).await.expect("Failed to call open channel").into_inner();
            while let Some(message) = update_stream.message().await.expect("failed to receive update") {
                assert_eq!(message.pending_chan_id, chid);
                if let Some(update) = message.update {
                    use tonic_lnd::rpc::open_status_update::Update;
                    match update {
                        Update::PsbtFund(ready) => {
                            let mut bytes = &*ready.psbt;
                            let tx = PartiallySignedTransaction::consensus_decode(&mut bytes).unwrap();
                            assert_eq!(tx.global.unsigned_tx.output.len(), 1);
                            // TODO: insert at random position
                            psbt.global.unsigned_tx.output.extend(tx.global.unsigned_tx.output);
                            let mut psbt_bytes = Vec::new();
                            psbt.consensus_encode(&mut psbt_bytes).unwrap();

                            let psbt_verify = tonic_lnd::rpc::FundingPsbtVerify {
                                pending_chan_id: Vec::from(&chid as &[_]),
                                funded_psbt: psbt_bytes.clone(),
                                skip_finalize: true,
                            };
                            let transition_msg = tonic_lnd::rpc::FundingTransitionMsg {
                                trigger: Some(tonic_lnd::rpc::funding_transition_msg::Trigger::PsbtVerify(psbt_verify)),
                            };
                            lnd.funding_state_step(transition_msg).await.expect("failed to execute funding state step");
                            // Reset transaction state to be non-finalized
                            psbt = PartiallySignedTransaction::from_unsigned_tx(psbt.global.unsigned_tx).expect("resetting tx failed");
                            let mut psbt_bytes = Vec::new();
                            psbt.consensus_encode(&mut psbt_bytes).unwrap();
                            let psbt_bytes = base64::encode(psbt_bytes);
                            return Ok(Response::new(Body::from(psbt_bytes)));
                        },
                        // panic?
                        _ => break,
                    }
                }
            }

            Ok(Response::new(Body::empty()))
        },

        // Return the 404 Not Found for other routes.
        _ => {
            let mut not_found = Response::default();
            *not_found.status_mut() = StatusCode::NOT_FOUND;
            Ok(not_found)
        }
    }
}
