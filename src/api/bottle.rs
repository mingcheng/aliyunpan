use serde_json::{Value, json};

use crate::{
    client::{Client, Host},
    error::{Result, unexpected_response},
    models::{Bottle, BottleUserLimit},
};

impl Client {
    /// Draw a random lucky bottle containing a resource share.
    ///
    /// The server currently limits draws to ten per day. Use [`Self::get_bottle_user_limit`]
    /// to inspect the account's quota; this method does not query or cache it.
    /// Quota errors are returned unchanged. Transport errors, HTTP 429 and transient
    /// server errors are not retried, because another draw could consume more quota.
    /// Explicit token/signature rejection still triggers normal authentication recovery.
    pub async fn fish_bottle(&self) -> Result<Bottle> {
        let value: Value = self
            .request_with_retries(Host::Api, "/adrive/v1/bottle/fish", &json!({}), true, &[], 0)
            .await?;
        let bottle: Bottle =
            serde_json::from_value(value.clone()).map_err(|_| unexpected_response("fish_bottle", &value))?;
        if bottle.bottle_id == 0 || bottle.bottle_name.trim().is_empty() || bottle.share_id.trim().is_empty() {
            return Err(unexpected_response("fish_bottle", &value));
        }
        Ok(bottle)
    }

    /// Query current lucky-bottle creation and draw quotas without drawing a bottle.
    ///
    /// Quotas are controlled by the server and can change between this query and a draw,
    /// including when the account is used on another device.
    pub async fn get_bottle_user_limit(&self) -> Result<BottleUserLimit> {
        let value: Value = self.post("/adrive/v1/bottle/getUserLimit", &json!({})).await?;
        if [
            "createBottleLimit",
            "createBottleUsed",
            "fishBottleLimit",
            "fishBottleUsed",
        ]
        .iter()
        .any(|key| value.get(key).and_then(Value::as_u64).is_none())
        {
            return Err(unexpected_response("get_bottle_user_limit", &value));
        }
        serde_json::from_value(value.clone()).map_err(|_| unexpected_response("get_bottle_user_limit", &value))
    }
}
