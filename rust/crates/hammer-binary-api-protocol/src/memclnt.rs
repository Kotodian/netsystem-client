//! V2 client registration and lifecycle messages.

use serde::{Deserialize, Serialize};

use crate::api::{Api, Block, Field, Service, name_crc};
use crate::value::FixedString;

// These two IDs are the only bootstrap IDs an SHM client needs before it can
// import the server's message table. Every other ID is discovered by
// `NAME_CRC` after `memclnt_create_v2_reply`.
pub const MEMCLNT_CREATE_V2_ID: u16 = 25;
pub const MEMCLNT_CREATE_V2_REPLY_ID: u16 = 26;

const fn primitive(name: &'static str, field_type: &'static str, length: usize) -> Field {
    Field {
        name,
        field_type,
        block: None,
        length: Some(length),
        length_field: None,
    }
}

const CREATE_BLOCK: Block = Block::Fields(&[
    primitive("id", "u16", 1),
    primitive("context", "u32", 1),
    primitive("ctx_quota", "i32", 1),
    primitive("input_queue", "u64", 1),
    primitive("name", "string", 64),
    primitive("api_versions", "u32", 8),
    primitive("keepalive", "bool", 1),
]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemclntCreateV2 {
    pub id: u16,
    pub context: u32,
    pub ctx_quota: i32,
    pub input_queue: u64,
    pub name: FixedString<64>,
    pub api_versions: [u32; 8],
    pub keepalive: bool,
}

impl Api for MemclntCreateV2 {
    const NAME: &'static str = "memclnt_create_v2";
    const BLOCK: Block = CREATE_BLOCK;
    const CRC: u32 = CREATE_BLOCK.crc();
    const NAME_CRC: &'static str = {
        const BYTES: [u8; 26] = name_crc::<26>(MemclntCreateV2::NAME, MemclntCreateV2::CRC);
        match std::str::from_utf8(&BYTES) {
            Ok(value) => value,
            Err(_) => panic!("generated API identity is ASCII"),
        }
    };
    const SERVICE: Option<Service> = Some(Service {
        caller: Self::NAME,
        reply: Some(MemclntCreateV2Reply::NAME),
        stream: false,
        stream_message: None,
        events: &[],
    });

    fn set_request_header(&mut self, id: u16, _: u32, context: u32) {
        self.id = id;
        self.context = context;
    }

    fn context(&self) -> Option<u32> {
        Some(self.context)
    }
}

const CREATE_REPLY_BLOCK: Block = Block::Fields(&[
    primitive("id", "u16", 1),
    primitive("context", "u32", 1),
    primitive("response", "i32", 1),
    primitive("handle", "u64", 1),
    primitive("index", "u32", 1),
    primitive("message_table", "u64", 1),
]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemclntCreateV2Reply {
    pub id: u16,
    pub context: u32,
    pub response: i32,
    pub handle: u64,
    pub index: u32,
    pub message_table: u64,
}

impl Api for MemclntCreateV2Reply {
    const NAME: &'static str = "memclnt_create_v2_reply";
    const BLOCK: Block = CREATE_REPLY_BLOCK;
    const CRC: u32 = CREATE_REPLY_BLOCK.crc();
    const NAME_CRC: &'static str = {
        const BYTES: [u8; 32] =
            name_crc::<32>(MemclntCreateV2Reply::NAME, MemclntCreateV2Reply::CRC);
        match std::str::from_utf8(&BYTES) {
            Ok(value) => value,
            Err(_) => panic!("generated API identity is ASCII"),
        }
    };

    fn context(&self) -> Option<u32> {
        Some(self.context)
    }
}

const DELETE_BLOCK: Block = Block::Fields(&[
    primitive("id", "u16", 1),
    primitive("index", "u32", 1),
    primitive("handle", "u64", 1),
    primitive("do_cleanup", "bool", 1),
]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemclntDelete {
    pub id: u16,
    pub index: u32,
    pub handle: u64,
    pub do_cleanup: bool,
}

impl Api for MemclntDelete {
    const NAME: &'static str = "memclnt_delete";
    const BLOCK: Block = DELETE_BLOCK;
    const CRC: u32 = DELETE_BLOCK.crc();
    const NAME_CRC: &'static str = {
        const BYTES: [u8; 23] = name_crc::<23>(MemclntDelete::NAME, MemclntDelete::CRC);
        match std::str::from_utf8(&BYTES) {
            Ok(value) => value,
            Err(_) => panic!("generated API identity is ASCII"),
        }
    };
    const SERVICE: Option<Service> = Some(Service {
        caller: Self::NAME,
        reply: Some(MemclntDeleteReply::NAME),
        stream: false,
        stream_message: None,
        events: &[],
    });

    fn set_request_header(&mut self, id: u16, _: u32, _: u32) {
        self.id = id;
    }
}

const DELETE_REPLY_BLOCK: Block = Block::Fields(&[
    primitive("id", "u16", 1),
    primitive("response", "i32", 1),
    primitive("handle", "u64", 1),
]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemclntDeleteReply {
    pub id: u16,
    pub response: i32,
    pub handle: u64,
}

impl Api for MemclntDeleteReply {
    const NAME: &'static str = "memclnt_delete_reply";
    const BLOCK: Block = DELETE_REPLY_BLOCK;
    const CRC: u32 = DELETE_REPLY_BLOCK.crc();
    const NAME_CRC: &'static str = {
        const BYTES: [u8; 29] = name_crc::<29>(MemclntDeleteReply::NAME, MemclntDeleteReply::CRC);
        match std::str::from_utf8(&BYTES) {
            Ok(value) => value,
            Err(_) => panic!("generated API identity is ASCII"),
        }
    };
}

const KEEPALIVE_BLOCK: Block = Block::Fields(&[
    primitive("id", "u16", 1),
    primitive("client_index", "u32", 1),
    primitive("context", "u32", 1),
]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemclntKeepalive {
    pub id: u16,
    pub client_index: u32,
    pub context: u32,
}

impl Api for MemclntKeepalive {
    const NAME: &'static str = "memclnt_keepalive";
    const BLOCK: Block = KEEPALIVE_BLOCK;
    const CRC: u32 = KEEPALIVE_BLOCK.crc();
    const NAME_CRC: &'static str = {
        const BYTES: [u8; 26] = name_crc::<26>(MemclntKeepalive::NAME, MemclntKeepalive::CRC);
        match std::str::from_utf8(&BYTES) {
            Ok(value) => value,
            Err(_) => panic!("generated API identity is ASCII"),
        }
    };
    const SERVICE: Option<Service> = Some(Service {
        caller: Self::NAME,
        reply: Some(MemclntKeepaliveReply::NAME),
        stream: false,
        stream_message: None,
        events: &[],
    });

    fn set_request_header(&mut self, id: u16, client_index: u32, context: u32) {
        self.id = id;
        self.client_index = client_index;
        self.context = context;
    }

    fn context(&self) -> Option<u32> {
        Some(self.context)
    }
}

const KEEPALIVE_REPLY_BLOCK: Block = Block::Fields(&[
    primitive("id", "u16", 1),
    primitive("context", "u32", 1),
    primitive("retval", "i32", 1),
]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemclntKeepaliveReply {
    pub id: u16,
    pub context: u32,
    pub retval: i32,
}

impl Api for MemclntKeepaliveReply {
    const NAME: &'static str = "memclnt_keepalive_reply";
    const BLOCK: Block = KEEPALIVE_REPLY_BLOCK;
    const CRC: u32 = KEEPALIVE_REPLY_BLOCK.crc();
    const NAME_CRC: &'static str = {
        const BYTES: [u8; 32] =
            name_crc::<32>(MemclntKeepaliveReply::NAME, MemclntKeepaliveReply::CRC);
        match std::str::from_utf8(&BYTES) {
            Ok(value) => value,
            Err(_) => panic!("generated API identity is ASCII"),
        }
    };

    fn context(&self) -> Option<u32> {
        Some(self.context)
    }
}
