use std::io::{self, Write};

use bip_bencode::{BDecodeOpt, BencodeRef, BConvert};
use bytes::Bytes;
use nom::{IResult, be_u32, be_u8, ErrorKind};
use byteorder::{WriteBytesExt, BigEndian};

use message::{self, ExtendedMessage, PeerWireProtocolMessage, ExtendedType};
use message::bencode;
use message::bits_extension;
use protocol::{PeerProtocol};

const EXTENSION_HEADER_LEN: usize = message::HEADER_LEN + 1;

const REQUEST_MESSAGE_TYPE_ID: u8 = 0;
const DATA_MESSAGE_TYPE_ID:    u8 = 1;
const REJECT_MESSAGE_TYPE_ID:  u8 = 2;

const ROOT_ERROR_KEY: &'static str = "PeerExtensionProtocolMessage";

pub enum PeerExtensionProtocolMessage<P> where P: PeerProtocol {
    LtMetadata(LtMetadataMessage),
    //UtPex(UtPexMessage),
    Custom(P::ProtocolMessage)
}

impl<P> PeerExtensionProtocolMessage<P> where P: PeerProtocol {
    pub fn bytes_needed(bytes: &[u8]) -> io::Result<Option<usize>> {
        // Follows same length prefix logic as our normal wire protocol...
        PeerWireProtocolMessage::<P>::bytes_needed(bytes)
    }

    pub fn parse_bytes(bytes: Bytes, extended: &ExtendedMessage, custom_prot: &mut P) -> io::Result<PeerExtensionProtocolMessage<P>> {
        match parse_extensions(bytes, extended, custom_prot) {
            IResult::Done(_, result) => result,
            _                        => Err(io::Error::new(io::ErrorKind::Other, "Failed To Parse PeerExtensionProtocolMessage"))
        }
    }

    pub fn write_bytes<W>(&self, mut writer: W, extended: &ExtendedMessage, custom_prot: &mut P) -> io::Result<()>
        where W: Write
    {
        match self {
            &PeerExtensionProtocolMessage::LtMetadata(ref msg) => {
                let ext_id = if let Some(ext_id) = extended.query_id(&ExtendedType::LtMetadata) {
                    ext_id
                } else { return Err(io::Error::new(io::ErrorKind::Other, "Can't Send LtMetadataMessage As We Have No Id Mapping")) };

                let total_len = (2 + msg.message_size()) as u32;

                try!(message::write_length_id_pair(&mut writer, total_len, Some(bits_extension::EXTENDED_MESSAGE_ID)));
                try!(writer.write_u8(ext_id));

                msg.write_bytes(writer)
            },
            &PeerExtensionProtocolMessage::Custom(ref msg)     => custom_prot.write_bytes(msg, writer)
        }
    }

    pub fn message_size(&self, custom_prot: &mut P) -> usize {
        match self {
            &PeerExtensionProtocolMessage::LtMetadata(ref msg) => msg.message_size(),
            &PeerExtensionProtocolMessage::Custom(ref msg)     => custom_prot.message_size(&msg)
        }
    }
}

fn parse_extensions<P>(mut bytes: Bytes, extended: &ExtendedMessage, custom_prot: &mut P) -> IResult<(), io::Result<PeerExtensionProtocolMessage<P>>>
    where P: PeerProtocol {
    let header_bytes = bytes.clone();

    // Attempt to parse a built in message type, otherwise, see if it is an extension type.
    alt!((),
         ignore_input!(
             switch!(header_bytes.as_ref(), throwaway_input!(tuple!(be_u32, be_u8, be_u8)),
                (message_len, bits_extension::EXTENDED_MESSAGE_ID, message_id) =>
                    call!(parse_extensions_with_id, bytes.split_off(EXTENSION_HEADER_LEN).split_to(message_len as usize - 2), extended, message_id)
            )
         ) | map!(value!(custom_prot.parse_bytes(bytes)),
               |res_cust_ext| res_cust_ext.map(|cust_ext| PeerExtensionProtocolMessage::Custom(cust_ext)))
    )
}

fn parse_extensions_with_id<P>(_input: (), bytes: Bytes, extended: &ExtendedMessage, id: u8) -> IResult<(), io::Result<PeerExtensionProtocolMessage<P>>>
    where P: PeerProtocol {
    let lt_metadata_id = extended.query_id(&ExtendedType::LtMetadata);
    //let ut_pex_id = extended.query_id(&ExtendedType::UtPex);

    let result = if lt_metadata_id == Some(id) {
        LtMetadataMessage::parse_bytes(bytes)
                .map(|lt_metadata_msg| PeerExtensionProtocolMessage::LtMetadata(lt_metadata_msg))
    } else {
        Err(io::Error::new(io::ErrorKind::Other, format!("Unknown Id For PeerExtensionProtocolMessage: {}", id)))
    };

    IResult::Done((), result)
}

// ----------------------------------------------------------------------------//

pub enum LtMetadataMessage {
    Request(LtMetadataRequestMessage),
    Data(LtMetadataDataMessage),
    Reject(LtMetadataRejectMessage)
}

impl LtMetadataMessage {
    pub fn parse_bytes(mut bytes: Bytes) -> io::Result<LtMetadataMessage> {
        // Our bencode is pretty flat, and we dont want to enforce a full decode, as data
        // messages have the raw data appended outside of the bencode structure...
        let decode_opts = BDecodeOpt::new(1, false, false);

        match BencodeRef::decode(bytes.clone().as_ref(), decode_opts) {
            Ok(bencode) => {
                let bencode_dict = try!(bencode::CONVERT.convert_dict(&bencode, ROOT_ERROR_KEY));
                let msg_type = try!(bencode::parse_message_type(bencode_dict));
                let piece = try!(bencode::parse_piece_index(bencode_dict));

                let bencode_bytes = bytes.split_to(bencode.buffer().len());
                let extra_bytes = bytes;

                match msg_type {
                    REQUEST_MESSAGE_TYPE_ID => Ok(LtMetadataMessage::Request(LtMetadataRequestMessage::with_bytes(piece, bencode_bytes))),
                    REJECT_MESSAGE_TYPE_ID  => Ok(LtMetadataMessage::Reject(LtMetadataRejectMessage::with_bytes(piece, bencode_bytes))),
                    DATA_MESSAGE_TYPE_ID    => {
                        let total_size = try!(bencode::parse_total_size(bencode_dict));

                        Ok(LtMetadataMessage::Data(LtMetadataDataMessage::with_bytes(piece, total_size, extra_bytes, bencode_bytes)))
                    },
                    other => { return Err(io::Error::new(io::ErrorKind::Other, format!("Failed To Recognize Message Type For LtMetadataMessage: {}", msg_type))) }
                }
            },
            Err(_) => Err(io::Error::new(io::ErrorKind::Other, "Failed To Parse LtMetadataMessage As Bencode"))
        }
    }

    pub fn write_bytes<W>(&self, writer: W) -> io::Result<()>
        where W: Write
    {
        match self {
            &LtMetadataMessage::Request(ref request) => request.write_bytes(writer),
            &LtMetadataMessage::Data(ref data)       => data.write_bytes(writer),
            &LtMetadataMessage::Reject(ref reject)   => reject.write_bytes(writer),
        }
    }

    pub fn message_size(&self) -> usize {
        match self {
            &LtMetadataMessage::Request(ref request) => request.message_size(),
            &LtMetadataMessage::Data(ref data)       => data.message_size(),
            &LtMetadataMessage::Reject(ref reject)   => reject.message_size(),
        }
    }
}

// ----------------------------------------------------------------------------//

pub struct LtMetadataRequestMessage {
    piece: i64,
    bytes: Bytes
}

impl LtMetadataRequestMessage {
    pub fn new(piece: i64) -> LtMetadataRequestMessage {
        let encoded_bytes = (ben_map!{
            bencode::MESSAGE_TYPE_KEY => ben_int!(REQUEST_MESSAGE_TYPE_ID as i64),
            bencode::PIECE_INDEX_KEY  => ben_int!(piece)
        }).encode();
        
        let mut bytes = Bytes::with_capacity(encoded_bytes.len());
        bytes.extend_from_slice(&encoded_bytes[..]);

        LtMetadataRequestMessage::with_bytes(piece, bytes)
    }

    pub fn with_bytes(piece: i64, bytes: Bytes) -> LtMetadataRequestMessage {
        LtMetadataRequestMessage{ piece: piece, bytes: bytes }
    }

    pub fn write_bytes<W>(&self, mut writer: W) -> io::Result<()>
        where W: Write
    {
        writer.write_all(self.bytes.as_ref())
    }

    pub fn message_size(&self) -> usize {
        self.bytes.len()
    }
}

pub struct LtMetadataDataMessage {
    piece:      i64,
    total_size: i64,
    data:       Bytes,
    bytes:      Bytes
}

impl LtMetadataDataMessage {
    pub fn new(piece: i64, total_size: i64, data: Bytes) -> LtMetadataDataMessage {
        let encoded_bytes = (ben_map!{
            bencode::MESSAGE_TYPE_KEY => ben_int!(DATA_MESSAGE_TYPE_ID as i64),
            bencode::PIECE_INDEX_KEY  => ben_int!(piece),
            bencode::TOTAL_SIZE_KEY   => ben_int!(total_size)
        }).encode();

        let mut bytes = Bytes::with_capacity(encoded_bytes.len());
        bytes.extend_from_slice(&encoded_bytes[..]);

        LtMetadataDataMessage::with_bytes(piece, total_size, data, bytes)
    }

    pub fn with_bytes(piece: i64, total_size: i64, data: Bytes, bytes: Bytes) -> LtMetadataDataMessage {
        LtMetadataDataMessage{ piece: piece, total_size: total_size, data: data, bytes: bytes }
    }

    pub fn write_bytes<W>(&self, mut writer: W) -> io::Result<()>
        where W: Write
    {
        try!(writer.write_all(self.bytes.as_ref()));

        writer.write_all(self.data.as_ref())
    }

    pub fn message_size(&self) -> usize {
        self.bytes.len() + self.data.len()
    }
}

pub struct LtMetadataRejectMessage {
    piece: i64,
    bytes: Bytes
}

impl LtMetadataRejectMessage {
    pub fn new(piece: i64) -> LtMetadataRejectMessage {
        let encoded_bytes = (ben_map!{
            bencode::MESSAGE_TYPE_KEY => ben_int!(REJECT_MESSAGE_TYPE_ID as i64),
            bencode::PIECE_INDEX_KEY  => ben_int!(piece)
        }).encode();

        let mut bytes = Bytes::with_capacity(encoded_bytes.len());
        bytes.extend_from_slice(&encoded_bytes[..]);

        LtMetadataRejectMessage::with_bytes(piece, bytes)
    }

    pub fn with_bytes(piece: i64, bytes: Bytes) -> LtMetadataRejectMessage {
        LtMetadataRejectMessage{ piece: piece, bytes: bytes }
    }

    pub fn write_bytes<W>(&self, mut writer: W) -> io::Result<()>
        where W: Write
    {
        writer.write_all(self.bytes.as_ref())
    }

    pub fn message_size(&self) -> usize {
        self.bytes.len()
    }
}