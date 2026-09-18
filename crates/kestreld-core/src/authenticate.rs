//! The `AUTHENTICATE` command.
//!
//! Parsing and reassembly live in `kestreld-services`; this decides what the
//! outcome means for the connection.

use kestrel_proto::{Message, MessageBuf, numeric};
use kestreld_services::{AuthOutcome, Credentials, Step};

use crate::client::{Client, ClientId};
use crate::server::{Action, Server};

impl Server {
    pub(crate) fn cmd_authenticate(
        &mut self,
        id: ClientId,
        msg: &Message<'_>,
        out: &mut Vec<Action>,
    ) {
        // A client must have negotiated the capability first; otherwise it has
        // no way to be told the result.
        if !self.client(id).is_some_and(|c| c.has_cap("sasl")) {
            self.fail_sasl(id, "You must request the sasl capability first", out);
            return;
        }
        if self.client(id).is_some_and(Client::is_authenticated) {
            out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_SASLALREADY)
                    .trailing("You have already authenticated using SASL"),
            });
            return;
        }

        let Some(param) = msg.param(0) else {
            self.fail_sasl(id, "Invalid AUTHENTICATE", out);
            return;
        };

        let Some(step) = self
            .clients_mut()
            .get_mut(&id)
            .map(|client| client.sasl.step(param))
        else {
            return;
        };

        match step {
            Step::NeedMore => {}
            Step::Challenge(payload) => out.push(Action::Send {
                to: id,
                message: MessageBuf::new("AUTHENTICATE").param(payload),
            }),
            Step::Aborted => out.push(Action::Send {
                to: id,
                message: self
                    .numeric(id, numeric::ERR_SASLABORTED)
                    .trailing("SASL authentication aborted"),
            }),
            // The reason is deliberately not relayed: telling a client why it
            // failed distinguishes a bad password from an unknown account.
            Step::Failed(_) => self.fail_sasl(id, "SASL authentication failed", out),
            Step::Complete(credentials) => self.finish_sasl(id, &credentials, out),
        }
    }

    fn finish_sasl(&mut self, id: ClientId, credentials: &Credentials, out: &mut Vec<Action>) {
        let outcome = match credentials {
            Credentials::Plain {
                authcid, password, ..
            } => self.accounts().authenticate(authcid, password),
            Credentials::External { .. } => {
                match self.client(id).and_then(Client::certificate_fingerprint) {
                    Some(fingerprint) => {
                        let fingerprint = fingerprint.to_owned();
                        self.accounts().authenticate_certificate(&fingerprint)
                    }
                    // EXTERNAL without a client certificate cannot succeed.
                    None => AuthOutcome::Failure,
                }
            }
        };

        let AuthOutcome::Success(account) = outcome else {
            self.fail_sasl(id, "SASL authentication failed", out);
            return;
        };

        // An authzid asks to act as a different account, which needs a
        // privilege nothing grants yet. Refuse rather than quietly ignore it.
        if let Credentials::Plain { authzid, .. } = credentials
            && !authzid.is_empty()
            && !authzid.eq_ignore_ascii_case(&account)
        {
            self.fail_sasl(id, "Cannot authenticate as another account", out);
            return;
        }

        self.set_account(id, Some(account.clone()));
        // Anyone who can see this client and asked to be told now learns
        // which account is behind the nickname.
        self.announce_account(id, out);

        let mask = self.client(id).map(Client::mask).unwrap_or_default();
        out.push(Action::Send {
            to: id,
            message: self
                .numeric(id, numeric::RPL_LOGGEDIN)
                .param(mask)
                .param(account.clone())
                .trailing(format!(
                    "You are now logged in as {}",
                    String::from_utf8_lossy(&account)
                )),
        });
        out.push(Action::Send {
            to: id,
            message: self
                .numeric(id, numeric::RPL_SASLSUCCESS)
                .trailing("SASL authentication successful"),
        });
    }

    fn fail_sasl(&mut self, id: ClientId, reason: &str, out: &mut Vec<Action>) {
        if let Some(client) = self.clients_mut().get_mut(&id) {
            client.sasl.reset();
        }
        out.push(Action::Send {
            to: id,
            message: self
                .numeric(id, numeric::ERR_SASLFAIL)
                .trailing(reason.to_owned()),
        });
    }
}
