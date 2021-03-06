use diesel::prelude::*;
use keytree::Xprv;
use merlin::Transcript;
use rand::{self, Rng};

use rocket::http::Cookie;
use rocket::outcome::IntoOutcome;
use rocket::request::{self, FromRequest, Request};

use crate::account::{AccountRecord, Wallet};
use crate::db::DBConnection;
use crate::names;
use crate::schema::user_records;

/// Owner of the accounts and assets.
pub struct User {
    seed: String,
}

#[derive(Debug, Queryable, Insertable)]
pub struct UserRecord {
    pub id: String,
    pub seed: String,
    pub info_json: String,
}

impl User {
    /// Generates user ID out of the seed
    pub fn id(&self) -> String {
        let mut t = Transcript::new(b"zkvmdemo.user_id");
        t.append_message(b"seed", &self.seed.as_bytes());
        let mut id = [0u8; 10]; // unique enough for the demo
        t.challenge_bytes(b"user_id", &mut id[..]);
        hex::encode(&id)
    }

    /// Creates a random seed
    pub fn random() -> Self {
        Self {
            seed: Self::random_seed(),
        }
    }

    /// root key
    pub fn xprv(&self) -> Xprv {
        Xprv::from_seed(self.seed.as_bytes())
    }

    /// Root xprv for an account.
    pub fn account_xprv(&self, account_alias: &str) -> Xprv {
        self.xprv().derive_intermediate_key(|t| {
            t.append_message(b"account_alias", account_alias.as_bytes());
        })
    }

    /// Root xprv for an issued asset.
    pub fn issuance_xprv(&self, asset_alias: &str) -> Xprv {
        self.xprv().derive_intermediate_key(|t| {
            t.append_message(b"asset_alias", asset_alias.as_bytes());
        })
    }

    /// Creates a random seed
    fn random_seed() -> String {
        let buf = rand::thread_rng().gen::<[u8; 32]>();
        hex::encode(&buf)
    }

    // FIXME: add normal error handling here
    fn create_new_user_records(&self, dbconn: &DBConnection) {
        // Create a couple of accounts and fund one of them with XLM from the main Issuer

        let wallet1 = Wallet::new(self, names::random_name());
        let wallet2 = Wallet::new(self, names::random_name());

        let dbconn = &dbconn.0;
        dbconn
            .transaction::<(), diesel::result::Error, _>(|| {
                {
                    use crate::schema::user_records::dsl::*;

                    diesel::insert_into(user_records)
                        .values(&UserRecord::new(&self))
                        .execute(dbconn)
                        .expect("Cannot insert a new user (remembered in a cookie) to the DB");
                }

                // // TBD: find the issuer, make a payment of 10 XLM to the first wallet.
                // if let Some(root) = AccountRecord::find_root(dbconn) {
                //     let root = root.wallet();
                //     let xlm = AssetRecord::find_root_token(dbconn).expect("root token should exist");

                //     let sender = root;
                //     let recipient = wallet1;

                //     // recipient prepares a receiver
                //     let payment = zkvm::ClearValue {
                //         qty: 10,
                //         flv: xlm.flavor(),
                //     };
                //     let payment_receiver_witness = recipient.account.generate_receiver(payment);
                //     let payment_receiver = &payment_receiver_witness.receiver;

                //     // Note: at this point, recipient saves the increased seq #,
                //     // but since we are doing the exchange in one call, we'll skip it.

                //     // Sender prepares a tx
                //     let (tx, _txid, proofs, reply) = sender
                //         .prepare_payment_tx(&payment_receiver, &bp_gens)
                //         .expect("TBD: handle payment tx failure properly")

                //     // Note: at this point, sender reserves the utxos and saves its incremented seq # until sender ACK'd ReceiverReply,
                //     // but since we are doing the exchange in one call, we'll skip it.

                //     // Sender gives Recipient info to watch for tx.

                //     // In a real system we'd add pending_utxos here, but since we are not pruning the mempool,
                //     // we want to show these as contributing to the node's balance immediately.
                //     recipient.utxos.push(
                //         Utxo {
                //             receiver: payment_receiver.clone(),
                //             sequence: payment_receiver_witness.sequence,
                //             anchor: reply.anchor, // store anchor sent by Alice
                //             proof: utreexo::Proof::Transient,
                //         }
                //         .received(),
                //     );
                // }

                {
                    use crate::schema::account_records::dsl::*;

                    diesel::insert_into(account_records)
                        .values(&AccountRecord::new(&wallet1))
                        .execute(dbconn)
                        .expect("Cannot insert a new wallet #1 to the DB");

                    diesel::insert_into(account_records)
                        .values(&AccountRecord::new(&wallet2))
                        .execute(dbconn)
                        .expect("Cannot insert a new wallet #2 to the DB");
                }

                Ok(())
            })
            .expect("Creating new user should succeed")
    }
}

impl UserRecord {
    pub fn new(user: &User) -> Self {
        UserRecord {
            id: user.id(),
            seed: user.seed.clone(),
            info_json: "{}".to_string(),
        }
    }

    pub fn user(&self) -> User {
        User {
            seed: self.seed.clone(),
        }
    }
}

impl<'a, 'r> FromRequest<'a, 'r> for User {
    type Error = ();

    fn from_request(request: &'a Request<'r>) -> request::Outcome<User, ()> {
        let cookie_name = "user_seed";

        let dbconn = request.guard::<DBConnection>()?;

        let mut cookies = request.cookies();

        if let Some(cookie) = cookies.get(cookie_name) {
            let seed = cookie.value().to_string();
            let user = User { seed };
            {
                use crate::schema::user_records::dsl::*;

                // If we have reset DB, but user still has a cookie,
                // we need to re-add the entry to DB.
                if let Err(_e) = user_records
                    .filter(id.eq(&user.id()))
                    .first::<UserRecord>(&dbconn.0)
                {
                    user.create_new_user_records(&dbconn)
                }
            }
            Some(user)
        } else {
            // Insert new user record into DB.
            let user = User::random();
            user.create_new_user_records(&dbconn);

            cookies.add(
                Cookie::build(cookie_name, user.seed.clone())
                    .path("/")
                    .secure(false)
                    .max_age(time::Duration::days(366))
                    .finish(),
            );

            Some(user)
        }
        .or_forward(())
    }
}
