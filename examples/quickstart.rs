//! Usage: `cargo run --example quickstart -- <credentials.json> [refresh_token]`
//!
//! Provide a refresh token on the first run; rotated credentials are saved in credentials.json.

use aliyunpan::{Client, Config, Credentials, FileStore, TokenStore};

#[tokio::main(flavor = "current_thread")]
async fn main() -> aliyunpan::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "credentials.json".into());
    let store = FileStore::new(path);
    if let Some(token) = args.next() {
        store.save(&Credentials::from_refresh_token(token)?)?;
    }

    let client = Client::connect(Config::default(), store).await?;
    let user = client.get_user_info().await?;
    let space = client.get_personal_info().await?.personal_space_info;
    println!(
        "user {} used {} / {} bytes",
        user.user_id, space.used_size, space.total_size
    );

    let drive = client.default_drive_id().await;
    for item in client.list_all_files(&drive, "root").await? {
        let kind = if item.is_folder() { "dir " } else { "file" };
        println!("{kind} {:>12} {}", item.size, item.name);
    }
    Ok(())
}
