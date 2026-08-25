fn main() {
    // vcpkg's static ffmpeg port builds with --enable-mediafoundation, so
    // libavcodec.a's object files (mfenc.o, mf_utils.o -- the MF hardware
    // encoder path, which we never call) reference COM interface GUIDs that
    // ffmpeg-sys-next's own list of extra Windows system libs
    // (ole32/secur32/ws2_32/bcrypt/user32) doesn't cover, and with static
    // linking the linker still needs every referenced symbol resolved even
    // in code we never reach at runtime. Most of those GUIDs (e.g.
    // IID_IMFMediaEventGenerator, IID_IMFTransform) live in mfuuid.lib, but
    // IID_ICodecAPI is the one exception -- ICodecAPI predates Media
    // Foundation (it's a DirectShow-era codec configuration interface MF
    // reuses) and its GUID has always lived in strmiids.lib instead. This
    // matches how FFmpeg's own upstream build links mediafoundation support:
    // `-lmfplat -lmfuuid -lstrmiids`.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-lib=mfuuid");
        println!("cargo:rustc-link-lib=strmiids");
    }
}
