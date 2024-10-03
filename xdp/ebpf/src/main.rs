#![no_std]
#![no_main]

use aya_bpf::bindings::xdp_action;
use aya_bpf::macros::{map, xdp};
use aya_bpf::maps::{HashMap, XskMap};
use aya_bpf::programs::XdpContext;

// Never panic
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}

#[map]
static XDP_SOCKETS: XskMap = XskMap::with_max_entries(1024, 0);

#[map]
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
    let len = ctx.data_end() - ctx.data();
    // Safety: It was checked to be in XdpContext
    let _data = unsafe { core::slice::from_raw_parts_mut(start, len) };
    // TODO: parse data and check whether it is a valid UDP frame

    xdp_action::XDP_PASS
}
