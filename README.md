# simple-whipper

A small CD ripper for the Linux desktop. It reads audio CDs with **libcdio-paranoia**, through Rust bindings, and has an **iced** GUI.

- Pick the drive, the output folder and the drive's read offset in the GUI
- Rip the selected tracks to FLAC or WAV, with paranoia error correction
- Progress bars for the current track and the whole rip, plus live read statistics
- An **Open folder** button that opens the result in your file manager

```sh
nix develop
cargo run --release
```

Your user needs access to `/dev/sr0` (through the `cdrom`/`optical` group or a seat ACL).

📖 **Full documentation**, covering architecture, FFI details, offset math, the GUI and the complete annotated source: [docs/DOCUMENTATION.md](docs/DOCUMENTATION.md)
