use core::convert::{TryFrom, TryInto};
use std::collections::BTreeMap;

use bytes::Bytes;
use ipld_core::{cid::Cid, ipld::Ipld};
use quick_protobuf::sizeofs::{sizeof_len, sizeof_varint};
use quick_protobuf::{BytesReader, MessageRead, MessageWrite, Writer, WriterBackend};

use crate::Error;

/// A protobuf ipld link.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct PbLink {
    /// Content identifier.
    pub cid: Cid,
    /// Name of the link.
    pub name: Option<String>,
    /// Size of the data.
    pub size: Option<u64>,
}

/// A protobuf ipld node.
#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub struct PbNode {
    /// List of protobuf ipld links.
    pub links: Vec<PbLink>,
    /// Binary data blob.
    pub data: Option<Bytes>,
}

/// A protobuf that references an ipld node.
#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub(crate) struct PbNodeRef<'a> {
    links: Vec<PbLink>,
    data: Option<&'a [u8]>,
}

impl PbNode {
    /// Deserializes a `PbNode` from bytes.
    pub fn from_bytes(buf: Bytes) -> Result<Self, Error> {
        let mut reader = BytesReader::from_bytes(&buf);
        let node = PbNodeRef::from_reader(&mut reader, &buf)?;
        let data = node.data.map(|d| buf.slice_ref(d));

        Ok(PbNode {
            links: node.links,
            data,
        })
    }

    /// Serializes a `PbNode` to bytes.
    pub fn into_bytes(mut self) -> Vec<u8> {
        // Links must be strictly sorted by name before encoding, leaving stable
        // ordering where the names are the same (or absent).
        self.links.sort_by(|a, b| {
            let a = a.name.as_ref().map(|s| s.as_bytes()).unwrap_or(&[][..]);
            let b = b.name.as_ref().map(|s| s.as_bytes()).unwrap_or(&[][..]);
            a.cmp(b)
        });

        let mut buf = Vec::with_capacity(self.get_size());
        let mut writer = Writer::new(&mut buf);
        self.write_message(&mut writer)
            .expect("protobuf to be valid");
        buf
    }
}

impl PbNodeRef<'_> {
    /// Serializes a `PbNode` to bytes.
    pub fn into_bytes(mut self) -> Vec<u8> {
        // Links must be strictly sorted by name before encoding, leaving stable
        // ordering where the names are the same (or absent).
        self.links.sort_by(|a, b| {
            let a = a.name.as_ref().map(|s| s.as_bytes()).unwrap_or(&[][..]);
            let b = b.name.as_ref().map(|s| s.as_bytes()).unwrap_or(&[][..]);
            a.cmp(b)
        });

        let mut buf = Vec::with_capacity(self.get_size());
        let mut writer = Writer::new(&mut buf);
        self.write_message(&mut writer)
            .expect("protobuf to be valid");
        buf
    }
}

impl From<PbNode> for Ipld {
    fn from(node: PbNode) -> Self {
        let mut map = BTreeMap::<String, Ipld>::new();
        let links = node
            .links
            .into_iter()
            .map(|link| link.into())
            .collect::<Vec<Ipld>>();
        map.insert("Links".to_string(), links.into());
        if let Some(data) = node.data {
            map.insert("Data".to_string(), Ipld::Bytes(data.to_vec()));
        }
        map.into()
    }
}

impl From<PbLink> for Ipld {
    fn from(link: PbLink) -> Self {
        let mut map = BTreeMap::<String, Ipld>::new();
        map.insert("Hash".to_string(), link.cid.into());

        if let Some(name) = link.name {
            map.insert("Name".to_string(), name.into());
        }
        if let Some(size) = link.size {
            map.insert("Tsize".to_string(), size.into());
        }
        map.into()
    }
}

impl<'a> TryFrom<&'a Ipld> for PbNodeRef<'a> {
    type Error = Error;

    fn try_from(ipld: &'a Ipld) -> core::result::Result<Self, Self::Error> {
        let mut node = PbNodeRef::default();

        match ipld {
            Ipld::Map(map) => {
                if map.is_empty() {
                    return Err(Error::FromIpld(
                        "DAG-PB must contain links or data".to_string(),
                    ));
                }

                for (key, value) in map {
                    match (key.as_str(), value) {
                        ("Links", Ipld::List(links)) => {
                            let mut prev_name = "".to_string();
                            for link in links.iter() {
                                match link {
                                    Ipld::Map(_) => {
                                        let pb_link: PbLink = link.try_into()?;
                                        // Make sure the links are sorted correctly.
                                        if let Some(ref name) = pb_link.name {
                                            if name.as_bytes() < prev_name.as_bytes() {
                                                // This error message isn't ideal, but the important thing is
                                                // that it errors.
                                                return Err(Error::LinksWrongOrder);
                                            }
                                            prev_name.clone_from(name)
                                        }
                                        node.links.push(pb_link)
                                    }
                                    other => {
                                        return Err(Error::FromIpld(format!(
                                            "Link entries must be an IPLD map, found: {:?}",
                                            other
                                        )))
                                    }
                                }
                            }
                        }
                        ("Data", Ipld::Bytes(data)) => {
                            node.data = Some(&data[..]);
                        }
                        (_, _) => {
                            return Err(Error::FromIpld(
                                "IPLD cannot be converted into DAG-PB".to_string(),
                            ))
                        }
                    }
                }
            }
            other => {
                return Err(Error::FromIpld(format!(
                    "Node must be an IPLD map, found: {:?}",
                    other
                )))
            }
        }

        Ok(node)
    }
}

impl TryFrom<&Ipld> for PbLink {
    type Error = Error;

    fn try_from(ipld: &Ipld) -> core::result::Result<PbLink, Self::Error> {
        if let Ipld::Map(map) = ipld {
            let mut cid = None;
            let mut name = None;
            let mut size = None;
            for (key, value) in map {
                match key.as_str() {
                    "Hash" => {
                        cid = if let Ipld::Link(cid) = value {
                            Some(*cid)
                        } else {
                            return Err(Error::FromIpld(format!(
                                "`Hash` must be an IPLD link, found: {:?}",
                                value
                            )));
                        };
                    }
                    "Name" => {
                        name = if let Ipld::String(name) = value {
                            Some(name.clone())
                        } else {
                            return Err(Error::FromIpld(format!(
                                "`Name` must be an IPLD string, found: {:?}",
                                value
                            )));
                        }
                    }
                    "Tsize" => {
                        size = if let Ipld::Integer(size) = value {
                            Some(u64::try_from(*size).map_err(|_| {
                                Error::FromIpld(
                                    "`Tsize` must fit into a 64-bit integer".to_string(),
                                )
                            })?)
                        } else {
                            return Err(Error::FromIpld(format!(
                                "`Tsize` must be an IPLD integer, found: {:?}",
                                value
                            )))?;
                        }
                    }
                    other => {
                        return Err(Error::FromIpld(format!(
                            "Only `Hash`, `Name` and `Tsize` are allowed as keys, found: `{}`",
                            other
                        )));
                    }
                }
            }

            // Name and size are optional, CID is not.
            match cid {
                Some(cid) => Ok(PbLink { cid, name, size }),
                None => Err(Error::FromIpld("`Hash` must be set".to_string())),
            }
        } else {
            Err(Error::FromIpld("Links must be an IPLD map".to_string()))
        }
    }
}

impl<'a> MessageRead<'a> for PbLink {
    fn from_reader(r: &mut BytesReader, bytes: &'a [u8]) -> quick_protobuf::Result<Self> {
        let mut cid = None;
        let mut name = None;
        let mut size = None;

        while !r.is_eof() {
            match r.next_tag(bytes) {
                Ok(10) => {
                    let bytes = r.read_bytes(bytes)?;
                    cid = Some(
                        Cid::try_from(bytes)
                            .map_err(|e| quick_protobuf::Error::Message(e.to_string()))?,
                    );
                }
                Ok(18) => name = Some(r.read_string(bytes)?.to_string()),
                Ok(24) => size = Some(r.read_uint64(bytes)?),
                Ok(_) => {
                    return Err(quick_protobuf::Error::Message(
                        "unexpected bytes".to_string(),
                    ))
                }
                Err(e) => return Err(e),
            }
        }
        Ok(PbLink {
            cid: cid.ok_or_else(|| quick_protobuf::Error::Message("missing Hash".into()))?,
            name,
            size,
        })
    }
}

impl MessageWrite for PbLink {
    fn get_size(&self) -> usize {
        let mut size = 0;
        let l = self.cid.encoded_len();
        size += 1 + sizeof_len(l);

        if let Some(ref name) = self.name {
            size += 1 + sizeof_len(name.as_bytes().len());
        }

        if let Some(tsize) = self.size {
            size += 1 + sizeof_varint(tsize);
        }
        size
    }

    fn write_message<W: WriterBackend>(&self, w: &mut Writer<W>) -> quick_protobuf::Result<()> {
        let bytes = self.cid.to_bytes();
        w.write_with_tag(10, |w| w.write_bytes(&bytes))?;

        if let Some(ref name) = self.name {
            w.write_with_tag(18, |w| w.write_string(name))?;
        }
        if let Some(size) = self.size {
            w.write_with_tag(24, |w| w.write_uint64(size))?;
        }
        Ok(())
    }
}

impl<'a> MessageRead<'a> for PbNodeRef<'a> {
    fn from_reader(r: &mut BytesReader, bytes: &'a [u8]) -> quick_protobuf::Result<Self> {
        let mut msg = Self::default();
        let mut links_before_data = false;
        while !r.is_eof() {
            match r.next_tag(bytes) {
                Ok(18) => {
                    // Links and data might be in any order, but they may not be interleaved.
                    if links_before_data {
                        return Err(quick_protobuf::Error::Message(
                            "duplicate Links section".to_string(),
                        ));
                    }
                    msg.links.push(r.read_message::<PbLink>(bytes)?)
                }
                Ok(10) => {
                    msg.data = Some(r.read_bytes(bytes)?);
                    if !msg.links.is_empty() {
                        links_before_data = true
                    }
                }
                Ok(_) => {
                    return Err(quick_protobuf::Error::Message(
                        "unexpected bytes".to_string(),
                    ))
                }
                Err(e) => return Err(e),
            }
        }
        Ok(msg)
    }
}

impl MessageWrite for PbNode {
    fn get_size(&self) -> usize {
        let mut size = 0;
        if let Some(ref data) = self.data {
            size += 1 + sizeof_len(data.len());
        }

        size += self
            .links
            .iter()
            .map(|s| 1 + sizeof_len((s).get_size()))
            .sum::<usize>();

        size
    }

    fn write_message<W: WriterBackend>(&self, w: &mut Writer<W>) -> quick_protobuf::Result<()> {
        for s in &self.links {
            w.write_with_tag(18, |w| w.write_message(s))?;
        }

        if let Some(ref data) = self.data {
            w.write_with_tag(10, |w| w.write_bytes(data))?;
        }

        Ok(())
    }
}

impl MessageWrite for PbNodeRef<'_> {
    fn get_size(&self) -> usize {
        let mut size = 0;
        if let Some(data) = self.data {
            size += 1 + sizeof_len(data.len());
        }

        size += self
            .links
            .iter()
            .map(|s| 1 + sizeof_len((s).get_size()))
            .sum::<usize>();

        size
    }

    fn write_message<W: WriterBackend>(&self, w: &mut Writer<W>) -> quick_protobuf::Result<()> {
        for s in &self.links {
            w.write_with_tag(18, |w| w.write_message(s))?;
        }

        if let Some(data) = self.data {
            w.write_with_tag(10, |w| w.write_bytes(data))?;
        }

        Ok(())
    }
}
