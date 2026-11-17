//! Embeds assets/icon.ico as the exe icon, writing the .res directly (no rc.exe): MSVC links it as
//! is, the GNU toolchain converts it with windres first.

use std::{env, fs, path::Path, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    let ico = fs::read("assets/icon.ico").expect("assets/icon.ico");
    let out = env::var("OUT_DIR").unwrap();
    let res = Path::new(&out).join("icon.res");
    fs::write(&res, icon_res(&ico)).unwrap();
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins={}", res.display());
        return;
    }
    let obj = Path::new(&out).join("icon.o");
    let ok = Command::new("windres").args(["-J", "res", "-O", "coff", "-i"]).arg(&res).arg("-o").arg(&obj).status();
    if ok.is_ok_and(|s| s.success()) {
        println!("cargo:rustc-link-arg-bins={}", obj.display());
    } else {
        println!("cargo:warning=windres not found: building without an exe icon");
    }
}

/// .ico -> .res: one RT_ICON per image (ids 1..) plus an RT_GROUP_ICON (id 1) listing them.
fn icon_res(ico: &[u8]) -> Vec<u8> {
    let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes(ico[i..i + 4].try_into().unwrap());
    let n = u16_at(4) as usize;
    let mut out = vec![];
    resource(&mut out, 0, 0, &[]); // a .res file starts with an empty entry
    let mut group = vec![0, 0, 1, 0];
    group.extend((n as u16).to_le_bytes());
    for i in 0..n {
        let e = 6 + 16 * i;
        let (len, off) = (u32_at(e + 8) as usize, u32_at(e + 12) as usize);
        resource(&mut out, 3, i as u16 + 1, &ico[off..off + len]);
        group.extend(&ico[e..e + 12]); // width, height, colours, reserved, planes, bpp, size
        group.extend((i as u16 + 1).to_le_bytes());
    }
    resource(&mut out, 14, 1, &group);
    out
}

fn resource(out: &mut Vec<u8>, kind: u16, id: u16, data: &[u8]) {
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(32u32.to_le_bytes()); // header size
    for v in [0xFFFF, kind, 0xFFFF, id] {
        out.extend(v.to_le_bytes());
    }
    out.extend(0u32.to_le_bytes()); // data version
    out.extend(if kind == 0 { 0u16 } else { 0x1010 }.to_le_bytes()); // moveable | discardable
    out.extend(0u16.to_le_bytes()); // language: neutral
    out.extend([0u8; 8]); // version, characteristics
    out.extend(data);
    out.resize(out.len().next_multiple_of(4), 0);
}
