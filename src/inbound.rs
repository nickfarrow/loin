use hyper::{Body, Client, Uri};
use serde_derive::{Deserialize, Serialize};

// TODO return result here
async fn body_to_string(body: Body) -> String {
    let body_bytes = hyper::body::to_bytes(body).await.unwrap();
    String::from_utf8(body_bytes.to_vec()).unwrap()
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    price: u32,
    size: u32,
    duration: u32,
    address: String,
}

pub async fn get_quote(pubkey: &str, refund_address: &str) -> Result<Quote, hyper::http::Error> {
    let https = hyper_tls::HttpsConnector::new();
    let client = Client::builder().build::<_, hyper::Body>(https);
    // get address
    // suggest capacity
    let base_uri = format!(
        "https://nolooking.chaincase.app/api/getinbound?nodeid={}&capacity={}&duration={}&refund_address={}",
        pubkey, 1000000, 1, refund_address
    );
    let url: Uri = base_uri.parse().unwrap();
    let res = client.get(url).await.unwrap();
    let body_str = body_to_string(res.into_body()).await;
    dbg!(&body_str);
    let quote: Quote = serde_json::from_str(&body_str).unwrap();
    dbg!(&quote);
    Ok(quote)
}
