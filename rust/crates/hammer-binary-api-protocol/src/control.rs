//! Core control messages.

use serde::{Deserialize, Serialize};

use crate::api::{Api, Block, Field, Service, name_crc};

const fn scalar(name: &'static str, field_type: &'static str) -> Field {
    Field {
        name,
        field_type,
        block: None,
        length: None,
        length_field: None,
    }
}

const CONTROL_PING_BLOCK: Block =
    Block::Fields(&[scalar("client_index", "u32"), scalar("context", "u32")]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlPing {
    pub id: u16,
    pub client_index: u32,
    pub context: u32,
}

impl Api for ControlPing {
    const NAME: &'static str = "control_ping";
    const BLOCK: Block = CONTROL_PING_BLOCK;
    const CRC: u32 = CONTROL_PING_BLOCK.crc();
    const NAME_CRC: &'static str = {
        const BYTES: [u8; 21] = name_crc::<21>(ControlPing::NAME, ControlPing::CRC);
        match std::str::from_utf8(&BYTES) {
            Ok(value) => value,
            Err(_) => panic!("generated API identity is ASCII"),
        }
    };
    const SERVICE: Option<Service> = Some(Service {
        caller: Self::NAME,
        reply: Some(ControlPingReply::NAME),
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

const CONTROL_PING_REPLY_BLOCK: Block = Block::Fields(&[
    scalar("context", "u32"),
    scalar("retval", "i32"),
    scalar("client_index", "u32"),
    scalar("vpe_pid", "u32"),
]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlPingReply {
    pub id: u16,
    pub context: u32,
    pub retval: i32,
    pub client_index: u32,
    pub vpe_pid: u32,
}

impl Api for ControlPingReply {
    const NAME: &'static str = "control_ping_reply";
    const BLOCK: Block = CONTROL_PING_REPLY_BLOCK;
    const CRC: u32 = CONTROL_PING_REPLY_BLOCK.crc();
    const NAME_CRC: &'static str = {
        const BYTES: [u8; 27] = name_crc::<27>(ControlPingReply::NAME, ControlPingReply::CRC);
        match std::str::from_utf8(&BYTES) {
            Ok(value) => value,
            Err(_) => panic!("generated API identity is ASCII"),
        }
    };

    fn context(&self) -> Option<u32> {
        Some(self.context)
    }
}
