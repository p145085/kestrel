//! `REGISTER` — creating an account from a client.
//!
//! Follows IRCv3's `draft/account-registration`. Email verification is not
//! implemented, so the email parameter is accepted and discarded rather than
//! pretended at: a server that collects an address it never verifies has taken
//! personal data for nothing.

use kestrel_proto::{Message, MessageBuf, numeric};
use kestreld_services::{RegisterError, account};

use crate::client::{Client, ClientId};
use crate::server::{Action, Server};

/// Shortest password accepted at registration.
const MIN_PASSWORD_LEN: usize = 8;

impl Server {
    pub(crate) fn cmd_register(
        &mut self,
        id: ClientId,
        msg: &Message<'_>,
        now: u64,
        out: &mut Vec<Action>,
    ) {
        if self.client(id).is_some_and(Client::is_authenticated) {
            self.fail_register(
                id,
                "ALREADY_AUTHENTICATED",
                "You are already logged in",
                out,
            );
            return;
        }

        // REGISTER <accountname> <email> <password>
        let (Some(requested), Some(_email), Some(password)) =
            (msg.param(0), msg.param(1), msg.param(2))
        else {
            self.fail_register(id, "NEED_MORE_PARAMS", "Not enough parameters", out);
            return;
        };

        // `*` means "use my nickname", which is what most clients send.
        let name = if requested == b"*" {
            let Some(nick) = self.client(id).and_then(Client::nick) else {
                self.fail_register(id, "INVALID_ACCOUNT_NAME", "Choose a nickname first", out);
                return;
            };
            nick.to_vec()
        } else {
            requested.to_vec()
        };

        if !account::is_valid_name(&name) {
            self.fail_register(
                id,
                "INVALID_ACCOUNT_NAME",
                "That account name is not allowed",
                out,
            );
            return;
        }
        if password.len() < MIN_PASSWORD_LEN {
            self.fail_register(
                id,
                "UNACCEPTABLE_PASSWORD",
                "Password must be at least 8 characters",
                out,
            );
            return;
        }

        match self.accounts_mut().register(&name, password, now) {
            Ok(()) => {}
            Err(RegisterError::AlreadyExists) => {
                self.fail_register(id, "ACCOUNT_EXISTS", "That account already exists", out);
                return;
            }
            Err(RegisterError::InvalidName) => {
                self.fail_register(
                    id,
                    "INVALID_ACCOUNT_NAME",
                    "That account name is not allowed",
                    out,
                );
                return;
            }
            Err(RegisterError::Password(_)) => {
                self.fail_register(
                    id,
                    "UNACCEPTABLE_PASSWORD",
                    "That password cannot be used",
                    out,
                );
                return;
            }
        }
        self.mark_accounts_changed();

        // Registration logs you straight in; making someone authenticate
        // immediately after creating an account is a step with no purpose.
        self.set_account(id, Some(name.clone()));
        self.announce_account(id, out);

        out.push(Action::Send {
            to: id,
            message: MessageBuf::new("REGISTER")
                .source(self.server_name())
                .param("SUCCESS")
                .param(name.clone())
                .trailing("Account created"),
        });

        let mask = self.client(id).map(Client::mask).unwrap_or_default();
        out.push(Action::Send {
            to: id,
            message: self
                .numeric(id, numeric::RPL_LOGGEDIN)
                .param(mask)
                .param(name.clone())
                .trailing(format!(
                    "You are now logged in as {}",
                    String::from_utf8_lossy(&name)
                )),
        });
    }

    fn fail_register(&self, id: ClientId, code: &str, message: &str, out: &mut Vec<Action>) {
        out.push(Action::Send {
            to: id,
            message: MessageBuf::new("FAIL")
                .source(self.server_name())
                .param("REGISTER")
                .param(code.to_owned())
                .trailing(message.to_owned()),
        });
    }
}
