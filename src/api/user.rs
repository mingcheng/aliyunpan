use serde::de::IgnoredAny;
use serde_json::{Value, json};

use crate::{
    client::{Client, Host},
    error::{Result, unexpected_response},
    models::{AlbumsInfo, PersonalInfo, SboxInfo, SignInInfo, UserInfo, VipInfo},
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

    /// Call the daily sign-in endpoint with the account's bearer token.
    ///
    /// Returns the envelope's `result` after validating `success` (request success).
    /// [`SignInInfo::is_sign_in`] is the server's sign-in outcome flag. False can mean
    /// already signed in or an unsuccessful sign-in; it does not identify the cause.
    /// Request success alone does not confirm that this call completed a sign-in.
    /// This does not perform reward tasks or separately claim rewards.
    /// Reward/task status is not used to infer the sign-in outcome.
    pub async fn sign_in(&self) -> Result<SignInInfo> {
        let value: Value = self
            .post_unsigned(Host::Member, "/v2/activity/sign_in_info", &json!({}))
            .await?;
        if value.get("success").and_then(Value::as_bool) != Some(true)
            || value
                .get("result")
                .and_then(|result| result.get("isSignIn"))
                .and_then(Value::as_bool)
                .is_none()
        {
            return Err(unexpected_response("sign_in", &value));
        }
        serde_json::from_value(value["result"].clone()).map_err(|_| unexpected_response("sign_in", &value))
    }

    /// Revoke the current device session, potentially invalidating the refresh token. Untested; use with care.
    pub async fn device_logout(&self) -> Result<()> {
        let _: IgnoredAny = self.post("/users/v1/users/device_logout", &json!({})).await?;
        Ok(())
    }
}
