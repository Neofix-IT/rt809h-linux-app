//! Hardware check of the Microwire path: load the image and read a 93C46.
fn main() -> rt809h_core::Result<()> {
    env_logger_init();
    let mut p = rt809h_core::Programmer::connect_with_log(Box::new(|m| eprintln!("{m}")))?;
    let d = p.microwire_read(128, &mut |_, _| true)?;
    println!("{} bytes: {:02x?}", d.len(), &d[..32]);
    println!("logic now: {}", p.info.algorithm);
    Ok(())
}
fn env_logger_init() {}
