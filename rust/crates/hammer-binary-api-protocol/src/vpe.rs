//! VPE protocol declarations shared by the server and external clients.

use serde::{Deserialize, Serialize};

use crate::api::{Api, Block, Field, Service, name_crc};
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

const fn string(name: &'static str, length: usize) -> Field {
    Field {
        name,
        field_type: "string",
        block: None,
        length: Some(length),
        length_field: None,
    }
}

const SHOW_VERSION_BLOCK: Block =
    Block::Fields(&[scalar("client_index", "u32"), scalar("context", "u32")]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowVersion {
    pub id: u16,
    pub client_index: u32,
    pub context: u32,
}

impl Api for ShowVersion {
    const NAME: &'static str = "show_version";
    const BLOCK: Block = SHOW_VERSION_BLOCK;
    const CRC: u32 = SHOW_VERSION_BLOCK.crc();
    const NAME_CRC: &'static str = {
        const BYTES: [u8; 21] = name_crc::<21>(ShowVersion::NAME, ShowVersion::CRC);
        match std::str::from_utf8(&BYTES) {
            Ok(value) => value,
            Err(_) => panic!("generated API identity is ASCII"),
        }
    };
    const SERVICE: Option<Service> = Some(Service {
        caller: Self::NAME,
        reply: Some(ShowVersionReply::NAME),
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

const SHOW_VERSION_REPLY_BLOCK: Block = Block::Fields(&[
    scalar("context", "u32"),
    scalar("retval", "i32"),
    string("program", 32),
    string("version", 32),
    string("build_date", 32),
    string("build_directory", 256),
]);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowVersionReply {
    pub id: u16,
    pub context: u32,
    pub retval: i32,
    pub program: FixedString<32>,
    pub version: FixedString<32>,
    pub build_date: FixedString<32>,
    pub build_directory: FixedString<256>,
}

impl Api for ShowVersionReply {
    const NAME: &'static str = "show_version_reply";
    const BLOCK: Block = SHOW_VERSION_REPLY_BLOCK;
    const CRC: u32 = SHOW_VERSION_REPLY_BLOCK.crc();
    const NAME_CRC: &'static str = {
        const BYTES: [u8; 27] = name_crc::<27>(ShowVersionReply::NAME, ShowVersionReply::CRC);
        match std::str::from_utf8(&BYTES) {
            Ok(value) => value,
            Err(_) => panic!("generated API identity is ASCII"),
        }
    };

    fn context(&self) -> Option<u32> {
        Some(self.context)
    }
}
