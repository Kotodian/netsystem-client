//! VPE-owned Binary API operations.

use hammer_binary_api_client::{Client, Error as ClientError};
use hammer_binary_api_protocol::vpe::{ShowVersion, ShowVersionReply};

#[derive(Debug, thiserror::Error)]
pub enum VpeError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("VPE rejected show_version with retval {retval}")]
    Rejected { retval: i32 },
}

pub struct VpeService<'client> {
    client: &'client mut Client,
}

impl<'client> VpeService<'client> {
    pub fn new(client: &'client mut Client) -> Self {
        Self { client }
    }

    pub async fn show_version(&mut self) -> Result<ShowVersionReply, VpeError> {
        let request = ShowVersion {
            id: 0,
            client_index: 0,
            context: 0,
        };
        let reply = self
            .client
            .invoke::<ShowVersion, ShowVersionReply>(request)
            .await?;
        if reply.retval != 0 {
            return Err(VpeError::Rejected {
                retval: reply.retval,
            });
        }
        Ok(reply)
    }
}
