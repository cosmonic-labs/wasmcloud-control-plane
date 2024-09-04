wit_bindgen::generate!({world: "authorizer"});

use exports::wasmcloud::control::callout_authorizer::{Context, Decision, Error, Guest};

struct Component;

impl Guest for Component {
    fn authorized(ctx: Context) -> Result<Decision, Error> {
        Ok(Decision {
            allowed: true,
            role: None,
            metadata: None,
        })
    }
}

export!(Component);
