use snap_coin::{crypto::keys::Private};

fn main() {
    let private = Private::new_random();
    println!("Private: {:?}", private.dump_buf());
    println!("Public: {:?} {}", private.to_public().dump_buf(), private.to_public().dump_base36());
}