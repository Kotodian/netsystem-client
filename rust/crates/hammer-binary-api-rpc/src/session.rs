//! Session-owned Binary API operations.

use hammer_binary_api_client::{Client, Error as ClientError};
use hammer_binary_api_protocol::session::{
    AppNamespaceAddDel, AppNamespaceAddDelReply, AppNamespaceAddDelRetval, InterfaceIndex,
};
use hammer_binary_api_protocol::value::FixedString;

#[derive(Debug, thiserror::Error)]
pub enum AppNamespaceAddDelError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("app_namespace_add_del rejected with {retval:?}")]
    Rejected { retval: AppNamespaceAddDelRetval },
    #[error("app_namespace_add_del returned unknown retval {retval}")]
    UnknownRetval { retval: i32 },
}

pub struct NamespaceService<'client> {
    client: &'client mut Client,
}

impl<'client> NamespaceService<'client> {
    #[inline(always)]
    pub fn new(client: &'client mut Client) -> Self {
        Self { client }
    }

    pub async fn add_or_rebind(
        &mut self,
        secret: u64,
        sw_if_index: u32,
        ip4_fib_id: u32,
        ip6_fib_id: u32,
        namespace_id: FixedString<64>,
    ) -> Result<u32, AppNamespaceAddDelError> {
        let request = AppNamespaceAddDel {
            id: 0,
            client_index: 0,
            context: 0,
            secret,
            is_add: true,
            sw_if_index: InterfaceIndex(sw_if_index),
            ip4_fib_id,
            ip6_fib_id,
            namespace_id,
            sock_name: String::new(),
        };
        let reply = self
            .client
            .invoke::<AppNamespaceAddDel, AppNamespaceAddDelReply>(request)
            .await?;
        namespace_index(reply)
    }

    pub async fn delete(
        &mut self,
        namespace_id: FixedString<64>,
    ) -> Result<(), AppNamespaceAddDelError> {
        let request = AppNamespaceAddDel {
            id: 0,
            client_index: 0,
            context: 0,
            secret: 0,
            is_add: false,
            sw_if_index: InterfaceIndex(u32::MAX),
            ip4_fib_id: u32::MAX,
            ip6_fib_id: u32::MAX,
            namespace_id,
            sock_name: String::new(),
        };
        let reply = self
            .client
            .invoke::<AppNamespaceAddDel, AppNamespaceAddDelReply>(request)
            .await?;
        namespace_index(reply).map(|_| ())
    }
}

fn namespace_index(reply: AppNamespaceAddDelReply) -> Result<u32, AppNamespaceAddDelError> {
    if reply.retval == 0 {
        return Ok(reply.appns_index);
    }
    match AppNamespaceAddDelRetval::try_from(reply.retval) {
        Ok(retval) => Err(AppNamespaceAddDelError::Rejected { retval }),
        Err(retval) => Err(AppNamespaceAddDelError::UnknownRetval { retval }),
    }
}
