pub struct DecodeBuffer<'a> {
    bytes: &'a mut [u8],
    end: *mut u8,
}

#[derive(Clone, Copy, Debug)]
pub enum DecoderError {
    UnexpectedEof(usize),
    InvariantViolation(&'static str),
}

impl<'a> DecodeBuffer<'a> {
    #[inline]
    pub unsafe fn new(start: *mut u8, end: *mut u8) -> Self {
        let len = end as usize - start as usize;
        let bytes = core::slice::from_raw_parts_mut(start, len);
        Self { bytes, end }
    }

    #[inline]
    pub fn new_checked(bytes: &'a mut [u8], end: *mut u8) -> Result<Self, DecoderError> {
        if bytes.as_ptr() > end {
            return Err(DecoderError::UnexpectedEof(0));
        }
        Ok(Self { bytes, end })
    }

    #[inline]
    pub fn decode_slice(
        self,
        count: usize,
    ) -> Result<(DecodeBuffer<'a>, DecodeBuffer<'a>), DecoderError> {
        self.ensure_len(count)?;
        let end = self.end;
        let (slice, remaining) = self.bytes.split_at_mut(count);
        Ok((
            Self::new_checked(slice, end)?,
            Self::new_checked(remaining, end)?,
        ))
    }

    #[inline]
    pub fn ensure_len(&self, len: usize) -> Result<(), DecoderError> {
        if self.len() < len {
            Err(DecoderError::UnexpectedEof(len))
        } else {
            Ok(())
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    #[inline]
    pub fn into_bytes(self) -> &'a mut [u8] {
        self.bytes
    }
}
