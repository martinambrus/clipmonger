//! Print what `MacTransfer` offers and reads for the current pasteboard.
use clipmonger_platform::traits::ClipboardTransfer as _;

fn main() {
    let transfer = clipmonger_backend_macos::MacTransfer::new().expect("transfer");
    let formats = transfer.formats().expect("formats");
    println!("formats: {formats:?}");
    for mime in formats {
        match transfer.read(&mime, 64 << 20) {
            Ok(bytes) => println!("{mime}: {} bytes", bytes.len()),
            Err(e) => println!("{mime}: error {e}"),
        }
    }
}
