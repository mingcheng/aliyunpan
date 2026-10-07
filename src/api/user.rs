use serde::de::IgnoredAny;
use serde_json::json;

use crate::{
    client::{Client, Host, SessionResult},
    error::Result,
    models::{AlbumsInfo, PersonalInfo, SboxInfo, UserInfo, VipInfo},
};

impl Client {
    pub async fn get_user_info(&self) -> Result<UserInfo> {
        self.post_unsigned(Host::User, "/v2/user/get", &json!({})).await
    }

    pub async fn get_personal_info(&self) -> Result<PersonalInfo> {
        self.post_unsigned(Host::Api, "/v2/databox/get_personal_info", &json!({}))
            .await
    }

    pub async fn get_sbox_info(&self) -> Result<SboxInfo> {
        self.post_unsigned(Host::Api, "/v2/sbox/get", &json!({})).await
    }

    pub async fn get_albums_info(&self) -> Result<AlbumsInfo> {
        self.post_unsigned(Host::Api, "/adrive/v1/user/albums_info", &json!({}))
            .await
    }

    pub async fn get_vip_info(&self) -> Result<VipInfo> {
        self.post_unsigned(Host::Api, "/business/v1.0/users/vip/info", &json!({}))
            .await
    }

    /// Renew the device session; not verified against the live API.
    pub async fn renew_session(&self) -> Result<()> {
        self.post::<SessionResult>("/users/v1/users/device/renew_session", &json!({}))
            .await?
            .into_result()
    }

    /// Revoke the current device session, potentially invalidating the refresh token. Untested; use with care.
    pub async fn device_logout(&self) -> Result<()> {
        let _: IgnoredAny = self.post("/users/v1/users/device_logout", &json!({})).await?;
        Ok(())
    }
}
