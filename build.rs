fn main() {
    // vcpkg's static ffmpeg port builds with --enable-mediafoundation, so
    // libavcodec.a's object files (mfenc.o, mf_utils.o -- the MF hardware
    // encoder path, which we never call) reference COM interface GUIDs
    // (IID_ICodecAPI, IID_IMFMediaEventGenerator, IID_IMFTransform) that live
    // in mfuuid.lib. ffmpeg-sys-next's own list of extra Windows system libs
    // to link (ole32/secur32/ws2_32/bcrypt/user32) doesn't include it, and
    // with static linking the linker still needs every referenced symbol
    // resolved even in code we never reach at runtime.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-lib=mfuuid");
    }
}
