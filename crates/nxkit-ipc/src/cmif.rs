//! CMIF, the request format inside the HIPC raw data: the `SFCI` header, the domain header and the control commands.
//!
//! A request in a session has the 16-byte `SFCI` header (magic, version, command, token) and then the command data.
//! A request in a domain has a 16-byte domain header before that: the type (1 send, 2 close), the number of objects
//! passed in, the size of the `SFCI` header plus the data, the object ID and a token. The IDs of the objects passed
//! in follow the data. A reply in a domain has `{ number of objects, 3 words }` before the `SFCO` header and the IDs
//! of the objects returned after the data. Checked on 2026-11-07 against `dm/host/hle_sf.c` of nxnative, which parses
//! the same headers.

/// The `SFCI` magic as a little-endian word.
const SFCI: u32 = 0x4943_4653;

/// The domain message type that sends a request to an object.
const DOMAIN_SEND: u8 = 1;
/// The domain message type that closes an object.
const DOMAIN_CLOSE: u8 = 2;

/// `ConvertCurrentObjectToDomain`.
pub(crate) const CONTROL_CONVERT_TO_DOMAIN: u32 = 0;
/// `CopyFromCurrentDomain`.
pub(crate) const CONTROL_COPY_FROM_DOMAIN: u32 = 1;
/// `CloneCurrentObject`.
pub(crate) const CONTROL_CLONE: u32 = 2;
/// `QueryPointerBufferSize`.
pub(crate) const CONTROL_QUERY_POINTER_BUFFER_SIZE: u32 = 3;
/// `CloneCurrentObjectEx`.
pub(crate) const CONTROL_CLONE_EX: u32 = 4;

/// Why the CMIF part of a message could not be parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CmifError {
    /// The raw data is shorter than its headers say.
    Truncated,
    /// The header does not start with `SFCI`.
    BadMagic(u32),
    /// The domain message type is neither send nor close.
    BadDomainType(u8),
}

impl std::fmt::Display for CmifError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CmifError::Truncated => f.write_str("the CMIF data is shorter than its headers say"),
            CmifError::BadMagic(m) => {
                write!(f, "the CMIF header has the magic {m:#x} instead of SFCI")
            }
            CmifError::BadDomainType(t) => write!(f, "the domain message has the unknown type {t}"),
        }
    }
}

/// One parsed CMIF request.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Call<'a> {
    /// The command ID. Zero for a close.
    pub(crate) command: u32,
    /// The data after the `SFCI` header.
    pub(crate) data: &'a [u8],
    /// The object ID the request is for. Zero outside a domain.
    pub(crate) object_id: u32,
    /// The IDs of the objects passed in.
    pub(crate) in_objects: Vec<u32>,
    /// The request closes the object instead of calling it.
    pub(crate) close: bool,
}

fn word(b: &[u8], at: usize) -> Result<u32, CmifError> {
    let s = b.get(at..at + 4).ok_or(CmifError::Truncated)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn header(b: &[u8]) -> Result<u32, CmifError> {
    let magic = word(b, 0)?;
    if magic != SFCI {
        return Err(CmifError::BadMagic(magic));
    }
    word(b, 8)
}

/// Parses a request of a session that is not a domain, and a control request.
pub(crate) fn parse_session(raw: &[u8]) -> Result<Call<'_>, CmifError> {
    let command = header(raw)?;
    Ok(Call {
        command,
        data: raw.get(16..).ok_or(CmifError::Truncated)?,
        object_id: 0,
        in_objects: Vec::new(),
        close: false,
    })
}

/// Parses a request of a session that is a domain.
pub(crate) fn parse_domain(raw: &[u8]) -> Result<Call<'_>, CmifError> {
    let kind = *raw.first().ok_or(CmifError::Truncated)?;
    let n_objects = usize::from(*raw.get(1).ok_or(CmifError::Truncated)?);
    let data_size = usize::from(u16::from_le_bytes([
        *raw.get(2).ok_or(CmifError::Truncated)?,
        *raw.get(3).ok_or(CmifError::Truncated)?,
    ]));
    let object_id = word(raw, 4)?;
    match kind {
        DOMAIN_CLOSE => Ok(Call {
            command: 0,
            data: &[],
            object_id,
            in_objects: Vec::new(),
            close: true,
        }),
        DOMAIN_SEND => {
            let body = raw.get(16..).ok_or(CmifError::Truncated)?;
            let command = header(body)?;
            if data_size < 16 || body.len() < data_size {
                return Err(CmifError::Truncated);
            }
            let mut in_objects = Vec::with_capacity(n_objects);
            for i in 0..n_objects {
                in_objects.push(word(body, data_size + i * 4)?);
            }
            Ok(Call {
                command,
                data: &body[16..data_size],
                object_id,
                in_objects,
                close: false,
            })
        }
        other => Err(CmifError::BadDomainType(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_bytes(command: u32) -> Vec<u8> {
        let mut v = Vec::new();
        for w in [SFCI, 0, command, 0] {
            v.extend_from_slice(&w.to_le_bytes());
        }
        v
    }

    #[test]
    fn a_session_request_has_its_command_and_data() {
        let mut raw = header_bytes(18);
        raw.extend_from_slice(&[9, 8, 7, 6]);
        let c = parse_session(&raw).unwrap();
        assert_eq!(c.command, 18);
        assert_eq!(c.data, [9, 8, 7, 6]);
    }

    #[test]
    fn a_wrong_magic_is_an_error() {
        let mut raw = header_bytes(1);
        raw[0] = b'X';
        assert!(matches!(parse_session(&raw), Err(CmifError::BadMagic(_))));
    }

    #[test]
    fn a_domain_request_has_its_object_and_the_objects_passed_in() {
        let mut raw = vec![DOMAIN_SEND, 1, 20, 0];
        raw.extend_from_slice(&5u32.to_le_bytes());
        raw.extend_from_slice(&[0; 8]);
        raw.extend(header_bytes(3));
        raw.extend_from_slice(&[1, 2, 3, 4]);
        raw.extend_from_slice(&7u32.to_le_bytes());
        let c = parse_domain(&raw).unwrap();
        assert_eq!((c.command, c.object_id, c.close), (3, 5, false));
        assert_eq!(c.data, [1, 2, 3, 4]);
        assert_eq!(c.in_objects, [7]);
    }

    #[test]
    fn a_domain_close_names_the_object() {
        let mut raw = vec![DOMAIN_CLOSE, 0, 0, 0];
        raw.extend_from_slice(&9u32.to_le_bytes());
        raw.extend_from_slice(&[0; 8]);
        let c = parse_domain(&raw).unwrap();
        assert!(c.close);
        assert_eq!(c.object_id, 9);
    }
}
