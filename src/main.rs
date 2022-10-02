#[macro_use]
extern crate rocket;
pub mod bitwarden;
pub mod prometheus;
use kube::Client;
use serde::Deserialize;
#[derive(Deserialize, Debug)]
pub struct Configuration {
    #[serde(default = "default_folder")]
    folder: String,
    #[serde(default = "default_interval")]
    interval: u64,
}

fn default_folder() -> String {
    "kubernetes".to_string()
}

fn default_interval() -> u64 {
    60 * 5
}

// fn write_file(path: String, content: String) -> std::io::Result<()> {
//     let mut file = File::create(path)?;
//     file.write_all(content.as_bytes())?;
//     Ok(())
// }

#[launch]
async fn rocket() -> _ {
    tracing_subscriber::fmt::init();
    let client = Client::try_default().await.unwrap();

    let config = envy::prefixed("BITWARDEN_SECRETS_OPERATOR_")
        .from_env::<Configuration>()
        .expect("could not parse configuration");

    // Generate and serialize the CRD
    // write_file(
    //     "deploy/crd.yaml".to_string(),
    //     serde_yaml::to_string(&BitwardenSecret::crd())?,
    // )?;

    info!("starting bitwarden-secretes-operator");
    tokio::spawn(async move {
        bitwarden::run(client, config).await.unwrap();
    });
    info!("controller terminated");

    // serve prometheus metrics
    rocket::build().mount("/", routes![prometheus::gather_metrics])
}
