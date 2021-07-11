use flowy_derive::{ProtoBuf, ProtoBuf_Enum};

#[derive(Debug, ProtoBuf_Enum)]
pub enum UserStatus {
    Unknown = 0,
    Login   = 1,
    Expired = 2,
}

impl std::default::Default for UserStatus {
    fn default() -> Self { UserStatus::Unknown }
}

#[derive(ProtoBuf, Default, Debug)]
pub struct UserDetail {
    #[pb(index = 1)]
    pub email: String,

    #[pb(index = 2)]
    pub name: String,

    #[pb(index = 3)]
    pub status: UserStatus,
}

use crate::sql_tables::User;
impl std::convert::From<User> for UserDetail {
    fn from(user: User) -> Self {
        UserDetail {
            email: user.email,
            name: user.name,
            status: UserStatus::Login,
        }
    }
}