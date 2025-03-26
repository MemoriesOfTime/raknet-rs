#![no_std]
#![no_main]
#![allow(unused)]

use aya_bpf::bindings::xdp_action;
use aya_bpf::macros::{map, xdp};
use aya_bpf::maps::{HashMap, XskMap};
use aya_bpf::programs::XdpContext;
use buffer::{DecodeBuffer, DecoderError};
use inet::ethernet::{self, EtherType};
use inet::{ip, ipv4, ipv6, udp};

mod buffer;
mod inet;

// Never panic
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}

#[map(name = "RAKNET_XDP_SOCKETS")]
static XDP_SOCKETS: XskMap = XskMap::with_max_entries(1024, 0);

#[map(name = "RAKNET_XDP_PORTS")]
static XDP_PORTS: HashMap<u16, u8> = HashMap::with_max_entries(1024, 0);

#[xdp]
pub fn raknet_xdp(ctx: XdpContext) -> u32 {
    let action = redirect_udp(&ctx);

    #[cfg(feature = "trace")]
    {
        use aya_log_ebpf as log;

        match action {
            xdp_action::XDP_DROP => log::trace!(&ctx, "ACTION: DROP"),
            xdp_action::XDP_PASS => log::trace!(&ctx, "ACTION: PASS"),
            xdp_action::XDP_REDIRECT => log::trace!(&ctx, "ACTION: REDIRECT"),
            xdp_action::XDP_ABORTED => log::trace!(&ctx, "ACTION: ABORTED"),
            _ => (),
        }
    }

    action
}

fn redirect_udp(ctx: &XdpContext) -> u32 {
    let start = ctx.data() as *mut u8;
    let end = ctx.data_end() as *mut u8;
    let buffer = unsafe {
        // Safety: start and end come from the caller and have been validated
        DecodeBuffer::new(start, end)
    };
    match decode(buffer) {
        Ok(Some(payload)) => {
            // if the payload is empty there isn't much we can do with it
            if payload.is_empty() {
                return xdp_action::XDP_DROP;
            }

            // if the packet is valid forward it on to the associated AF_XDP socket
            let queue_id = unsafe { (*ctx.ctx).rx_queue_index };
            XDP_SOCKETS
                .redirect(queue_id, 0)
                .unwrap_or(xdp_action::XDP_PASS)
        }
        Ok(None) => xdp_action::XDP_PASS,
        Err(_) => xdp_action::XDP_ABORTED,
    }
}

#[inline]
fn decode(buffer: DecodeBuffer<'_>) -> Result<Option<DecodeBuffer<'_>>, DecoderError> {
    let (header, buffer) = ethernet::Header::decode(buffer)?;
    match *header.ethertype() {
        EtherType::IPV4 => decode_ipv4(buffer),
        EtherType::IPV6 => decode_ipv6(buffer),
        // pass the packet on to the OS network stack if we don't understand it
        _ => Ok(None),
    }
}

#[inline]
fn decode_ipv4(buffer: DecodeBuffer<'_>) -> Result<Option<DecodeBuffer<'_>>, DecoderError> {
    let (header, buffer) = ipv4::Header::decode(buffer)?;
    let protocol = header.protocol();

    //= https://www.rfc-editor.org/rfc/rfc791#section-3.1
    //# IHL:  4 bits
    //#
    //# Internet Header Length is the length of the internet header in 32
    //# bit words, and thus points to the beginning of the data.  Note that
    //# the minimum value for a correct header is 5.

    // subtract the fixed header size
    let count_without_header = header
        .vihl()
        .header_len()
        .checked_sub(5)
        .ok_or(DecoderError::InvariantViolation("invalid IPv4 IHL value"))?;

    // skip the options and go to the actual payload
    let options_len = count_without_header as usize * (32 / 8);
    let (_options, buffer) = buffer.decode_slice(options_len)?;

    parse_ip_protocol(*protocol, buffer)
}

#[inline]
fn decode_ipv6(buffer: DecodeBuffer<'_>) -> Result<Option<DecodeBuffer<'_>>, DecoderError> {
    let (header, buffer) = ipv6::Header::decode(buffer)?;
    let protocol = header.next_header();

    // TODO parse Hop-by-hop/Options headers, for now we'll just forward the packet on to the OS

    parse_ip_protocol(*protocol, buffer)
}

#[inline]
fn parse_ip_protocol(
    protocol: ip::Protocol,
    buffer: DecodeBuffer<'_>,
) -> Result<Option<DecodeBuffer<'_>>, DecoderError> {
    match protocol {
        ip::Protocol::UDP | ip::Protocol::UDPLITE => parse_udp(buffer),
        // pass the packet on to the OS network stack if we don't understand it
        _ => Ok(None),
    }
}

#[inline]
fn parse_udp(buffer: DecodeBuffer<'_>) -> Result<Option<DecodeBuffer<'_>>, DecoderError> {
    let (header, buffer) = udp::Header::decode(buffer)?;

    // Make sure the port is in the port map. Otherwise, forward the packet to the OS.
    if XDP_PORTS.get_ptr(&header.destination().get()).is_none() {
        return Ok(None);
    }

    // NOTE: duvet doesn't know how to parse this RFC since it doesn't follow more modern formatting
    //# https://www.rfc-editor.org/rfc/rfc768
    //# Length  is the length  in octets  of this user datagram  including  this
    //# header  and the data.   (This  means  the minimum value of the length is
    //# eight.)
    let total_len = header.len().get();
    let payload_len = total_len
        .checked_sub(8)
        .ok_or(DecoderError::InvariantViolation("invalid UDP length"))?;
    let (udp_payload, _remaining) = buffer.decode_slice(payload_len as usize)?;

    Ok(Some(udp_payload))
}
