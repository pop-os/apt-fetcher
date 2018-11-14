use libflate::gzip::Decoder as GzDecoder;
use lz4::Decoder as Lz4Decoder;
use std::io::{self, Read};
use xz2::read::XzDecoder;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Compression {
    None,
    Gz,
    Xz,
    Lz4
}

pub struct Decompressor {
    reader: Box<dyn Read + Send>
}

impl Decompressor {
    pub fn none<R: Read + Send + 'static>(reader: R) -> io::Result<Self> {
        Ok(Self { reader: Box::new(reader)} )
    }

    pub fn gz<R: Read + Send + 'static>(reader: R) -> io::Result<Self> {
        Ok(Self { reader: Box::new(GzDecoder::new(reader)?) })
    }

    pub fn lz4<R: Read + Send + 'static>(reader: R) -> io::Result<Self> {
        Ok(Self { reader: Box::new(Lz4Decoder::new(reader)?) })
    }

    pub fn xz<R: Read + Send + 'static>(reader: R) -> io::Result<Self> {
        Ok(Self { reader: Box::new(XzDecoder::new(reader)) })
    }

    pub fn from_variant<R: Read + Send + 'static>(reader: R, variant: Compression) -> io::Result<Self> {
        match variant {
            Compression::Xz => Decompressor::xz(reader),
            Compression::Gz => Decompressor::gz(reader),
            Compression::Lz4 => Decompressor::lz4(reader),
            Compression::None => Decompressor::none(reader),
        }
    }
}

impl Read for Decompressor {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf)
    }
}
