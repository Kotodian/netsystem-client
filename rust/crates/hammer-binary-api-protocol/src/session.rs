//! Session-owned Binary API declarations.

use serde::{Deserialize, Serialize};

use crate::api::{Api, Block, Field, Service, Typedef, name_crc};
use crate::value::FixedString;

const fn scalar(name: &'static str, field_type: &'static str) -> Field {
    Field {
        name,
        field_type,
        block: None,
        length: None,
        length_field: None,
    }
}

const fn declared(name: &'static str, field_type: &'static str, block: Block) -> Field {
    Field {
        name,
        field_type,
        block: Some(block),
        length: None,
        length_field: None,
    }
}

const fn string(name: &'static str, length: usize) -> Field {
    Field {
        name,
        field_type: "string",
        block: None,
        length: Some(length),
        length_field: None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceIndex(pub u32);

impl Typedef for InterfaceIndex {
    const NAME: &'static str = "interface_index";
    const BLOCK: Block = Block::Alias;
}

const APP_NAMESPACE_ADD_DEL_BLOCK: Block = Block::Fields(&[
    scalar("client_index", "u32"),
    scalar("context", "u32"),
    scalar("secret", "u64"),
    scalar("is_add", "bool"),
    declared("sw_if_index", InterfaceIndex::NAME, InterfaceIndex::BLOCK),
    scalar("ip4_fib_id", "u32"),
    scalar("ip6_fib_id", "u32"),
    string("namespace_id", 64),
    string("sock_name", 0),
]);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppNamespaceAddDel {
    pub id: u16,
    pub client_index: u32,
    pub context: u32,
    pub secret: u64,
    pub is_add: bool,
    pub sw_if_index: InterfaceIndex,
    pub ip4_fib_id: u32,
    pub ip6_fib_id: u32,
    pub namespace_id: FixedString<64>,
    pub sock_name: String,
}

impl Api for AppNamespaceAddDel {
    const NAME: &'static str = "app_namespace_add_del_v4";
    const BLOCK: Block = APP_NAMESPACE_ADD_DEL_BLOCK;
    const CRC: u32 = APP_NAMESPACE_ADD_DEL_BLOCK.crc();
    const NAME_CRC: &'static str = {
        const BYTES: [u8; 33] = name_crc::<33>(AppNamespaceAddDel::NAME, AppNamespaceAddDel::CRC);
        match std::str::from_utf8(&BYTES) {
            Ok(value) => value,
            Err(_) => panic!("generated API identity is ASCII"),
        }
    };
    const SERVICE: Option<Service> = Some(Service {
        caller: Self::NAME,
        reply: Some(AppNamespaceAddDelReply::NAME),
        stream: false,
        stream_message: None,
        events: &[],
    });
    const OPTIONS: &'static [(&'static str, Option<&'static str>)] = &[("deprecated", None)];

    fn set_request_header(&mut self, id: u16, client_index: u32, context: u32) {
        self.id = id;
        self.client_index = client_index;
        self.context = context;
    }

    fn context(&self) -> Option<u32> {
        Some(self.context)
    }
}

const APP_NAMESPACE_ADD_DEL_REPLY_BLOCK: Block = Block::Fields(&[
    scalar("context", "u32"),
    scalar("retval", "i32"),
    scalar("appns_index", "u32"),
]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppNamespaceAddDelReply {
    pub id: u16,
    pub context: u32,
    pub retval: i32,
    pub appns_index: u32,
}

impl Api for AppNamespaceAddDelReply {
    const NAME: &'static str = "app_namespace_add_del_v4_reply";
    const BLOCK: Block = APP_NAMESPACE_ADD_DEL_REPLY_BLOCK;
    const CRC: u32 = APP_NAMESPACE_ADD_DEL_REPLY_BLOCK.crc();
    const NAME_CRC: &'static str = {
        const BYTES: [u8; 39] =
            name_crc::<39>(AppNamespaceAddDelReply::NAME, AppNamespaceAddDelReply::CRC);
        match std::str::from_utf8(&BYTES) {
            Ok(value) => value,
            Err(_) => panic!("generated API identity is ASCII"),
        }
    };

    fn context(&self) -> Option<u32> {
        Some(self.context)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppNamespaceAddDelRetval {
    Invalid,
    NotSupported,
}

impl TryFrom<i32> for AppNamespaceAddDelRetval {
    type Error = i32;

    #[inline(always)]
    fn try_from(retval: i32) -> Result<Self, Self::Error> {
        match retval {
            -19 => Ok(Self::Invalid),
            -10 => Ok(Self::NotSupported),
            retval => Err(retval),
        }
    }
}
