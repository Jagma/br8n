use br8n::backup::crypto::{open_bytes, seal_bytes, Key};
use std::io::{Cursor, Read};

const CHUNK: usize = 1024 * 1024;

fn hash_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

fn multi_frame_plaintext() -> Vec<u8> {
    (0..(3 * CHUNK + 17)).map(|i| (i % 251) as u8).collect()
}

#[test]
fn a_key_round_trips_through_a_file_at_mode_600() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("backup.key");
    let key = Key::generate();
    key.save(&path).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a key readable by others is not a secret");
    }

    let loaded = Key::load(&path).unwrap();
    assert_eq!(loaded.to_hex(), key.to_hex());
    assert_eq!(loaded.id(), key.id());
}

#[test]
fn saving_over_an_existing_key_fails_and_leaves_the_original_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("backup.key");
    let original = Key::generate();
    original.save(&path).unwrap();
    let original_bytes = std::fs::read(&path).unwrap();

    let replacement = Key::generate();
    let err = replacement.save(&path).unwrap_err();
    assert!(
        err.to_string().contains("already exists") && err.to_string().contains("orphan"),
        "error must say a key already exists and that overwriting would orphan backups, got: {err}"
    );

    let bytes_after = std::fs::read(&path).unwrap();
    assert_eq!(original_bytes, bytes_after);
    assert_eq!(Key::load(&path).unwrap().to_hex(), original.to_hex());
}

#[test]
fn two_generated_keys_differ() {
    assert_ne!(Key::generate().to_hex(), Key::generate().to_hex());
}

#[test]
fn sealing_the_same_bytes_twice_yields_identical_ciphertext_so_dedup_survives_encryption() {
    let key = Key::generate();
    let plain = b"pgbouncer runs in transaction mode".to_vec();
    let h = hash_of(&plain);
    let a = seal_bytes(&key, &h, &plain).unwrap();
    let b = seal_bytes(&key, &h, &plain).unwrap();
    assert_eq!(a, b);
    assert_ne!(a, plain, "ciphertext must not be the plaintext");
}

#[test]
fn different_content_hashes_do_not_share_a_nonce() {
    let key = Key::generate();
    let plain = b"the same plaintext both times".to_vec();
    let c1 = seal_bytes(&key, "content-hash-a", &plain).unwrap();
    let c2 = seal_bytes(&key, "content-hash-b", &plain).unwrap();
    assert_ne!(c1, c2);
}

#[test]
fn open_recovers_the_plaintext() {
    let key = Key::generate();
    let plain = b"a moderately long note about connection pooling".to_vec();
    let h = hash_of(&plain);
    let sealed = seal_bytes(&key, &h, &plain).unwrap();
    assert_eq!(open_bytes(&key, &h, &sealed).unwrap(), plain);
}

#[test]
fn the_wrong_key_fails_cleanly_rather_than_panicking() {
    let key = Key::generate();
    let other = Key::generate();
    let plain = b"secret".to_vec();
    let h = hash_of(&plain);
    let sealed = seal_bytes(&key, &h, &plain).unwrap();
    let err = open_bytes(&other, &h, &sealed).unwrap_err();
    assert!(
        err.to_string().contains("decrypt"),
        "error must name the failure, got: {err}"
    );
}

#[test]
fn opening_plaintext_that_was_never_sealed_fails_cleanly_rather_than_panicking() {
    let key = Key::generate();
    let plain = b"just some plaintext that was never encrypted at all".to_vec();
    let h = hash_of(&plain);
    let err = open_bytes(&key, &h, &plain).unwrap_err();
    assert!(
        err.to_string().contains("bad magic"),
        "error must name the failure, got: {err}"
    );
}

#[test]
fn opening_bytes_with_the_wrong_magic_fails_cleanly_rather_than_panicking() {
    let key = Key::generate();
    let plain = b"secret".to_vec();
    let h = hash_of(&plain);
    let mut sealed = seal_bytes(&key, &h, &plain).unwrap();
    sealed[0..4].copy_from_slice(b"XXXX");
    let err = open_bytes(&key, &h, &sealed).unwrap_err();
    assert!(
        err.to_string().contains("bad magic"),
        "error must name the failure, got: {err}"
    );
}

#[test]
fn opening_a_buffer_too_short_to_hold_the_magic_fails_cleanly_rather_than_panicking() {
    let key = Key::generate();
    let sealed = vec![0u8; 2];
    let err = open_bytes(&key, "irrelevant-hash", &sealed).unwrap_err();
    assert!(
        err.to_string().contains("too short"),
        "error must name the failure, got: {err}"
    );
}

#[test]
fn a_payload_larger_than_one_chunk_round_trips_through_multi_frame_streaming() {
    let key = Key::generate();
    let plain = multi_frame_plaintext();
    let h = hash_of(&plain);
    let mut sealed = Vec::new();
    br8n::backup::crypto::seal_stream(&key, &h, &mut Cursor::new(&plain), &mut sealed).unwrap();
    let frames = plain.len().div_ceil(CHUNK);
    assert_eq!(sealed.len(), 4 + plain.len() + frames * 16);
    let mut out = Vec::new();
    br8n::backup::crypto::open_stream(&key, &h, &mut Cursor::new(&sealed), &mut out).unwrap();
    assert_eq!(out, plain);
}

#[test]
fn a_truncated_ciphertext_is_rejected() {
    let key = Key::generate();
    let plain = b"secret".to_vec();
    let h = hash_of(&plain);
    let mut sealed = seal_bytes(&key, &h, &plain).unwrap();
    sealed.truncate(sealed.len() - 1);
    assert!(open_bytes(&key, &h, &sealed).is_err());
}

#[test]
fn truncating_to_exactly_one_whole_sealed_frame_is_still_rejected() {
    let key = Key::generate();
    let plain = multi_frame_plaintext();
    let h = hash_of(&plain);
    let mut sealed = Vec::new();
    br8n::backup::crypto::seal_stream(&key, &h, &mut Cursor::new(&plain), &mut sealed).unwrap();
    sealed.truncate(4 + (CHUNK + 16));
    let mut out = Vec::new();
    assert!(
        br8n::backup::crypto::open_stream(&key, &h, &mut Cursor::new(&sealed), &mut out).is_err()
    );
}

#[test]
fn flipping_a_bit_mid_stream_is_rejected() {
    let key = Key::generate();
    let plain = multi_frame_plaintext();
    let h = hash_of(&plain);
    let mut sealed = Vec::new();
    br8n::backup::crypto::seal_stream(&key, &h, &mut Cursor::new(&plain), &mut sealed).unwrap();
    let mid = sealed.len() / 2;
    sealed[mid] ^= 0x01;
    let mut out = Vec::new();
    assert!(
        br8n::backup::crypto::open_stream(&key, &h, &mut Cursor::new(&sealed), &mut out).is_err()
    );
}

struct ShortReader<R> {
    inner: R,
    max_read: usize,
    interrupt_next: bool,
}

impl<R: Read> Read for ShortReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.interrupt_next {
            self.interrupt_next = false;
            return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
        }
        self.interrupt_next = true;
        let cap = self.max_read.min(buf.len());
        self.inner.read(&mut buf[..cap])
    }
}

#[test]
fn sealing_a_source_that_short_reads_matches_sealing_the_same_bytes_from_a_cursor() {
    let key = Key::generate();
    let plain = multi_frame_plaintext();
    let h = hash_of(&plain);

    let mut sealed_from_cursor = Vec::new();
    br8n::backup::crypto::seal_stream(&key, &h, &mut Cursor::new(&plain), &mut sealed_from_cursor)
        .unwrap();

    let mut short_reader = ShortReader {
        inner: Cursor::new(&plain),
        max_read: 3,
        interrupt_next: false,
    };
    let mut sealed_from_short_reads = Vec::new();
    br8n::backup::crypto::seal_stream(&key, &h, &mut short_reader, &mut sealed_from_short_reads)
        .unwrap();

    assert_eq!(sealed_from_cursor, sealed_from_short_reads);
}

#[test]
fn sealing_a_source_that_returns_interrupted_matches_sealing_the_same_bytes_from_a_cursor() {
    let key = Key::generate();
    let plain = multi_frame_plaintext();
    let h = hash_of(&plain);

    let mut sealed_from_cursor = Vec::new();
    br8n::backup::crypto::seal_stream(&key, &h, &mut Cursor::new(&plain), &mut sealed_from_cursor)
        .unwrap();

    let mut interrupting_reader = ShortReader {
        inner: Cursor::new(&plain),
        max_read: plain.len(),
        interrupt_next: true,
    };
    let mut sealed_from_interrupts = Vec::new();
    br8n::backup::crypto::seal_stream(
        &key,
        &h,
        &mut interrupting_reader,
        &mut sealed_from_interrupts,
    )
    .unwrap();

    assert_eq!(sealed_from_cursor, sealed_from_interrupts);
}

#[test]
fn from_hex_rejects_non_hex_input() {
    let err = Key::from_hex("this is not hex at all").err().unwrap();
    assert!(
        err.to_string().contains("hex"),
        "error must name the failure, got: {err}"
    );
}

#[test]
fn from_hex_rejects_correct_hex_of_the_wrong_length() {
    let err = Key::from_hex("00112233").err().unwrap();
    assert!(
        err.to_string().contains("32 bytes"),
        "error must name the failure, got: {err}"
    );
}

#[test]
fn a_payload_of_exactly_one_chunk_round_trips() {
    let key = Key::generate();
    let plain: Vec<u8> = (0..CHUNK).map(|i| (i % 251) as u8).collect();
    let h = hash_of(&plain);
    let sealed = seal_bytes(&key, &h, &plain).unwrap();
    assert_eq!(open_bytes(&key, &h, &sealed).unwrap(), plain);
}

#[test]
fn a_zero_byte_payload_round_trips() {
    let key = Key::generate();
    let plain: Vec<u8> = Vec::new();
    let h = hash_of(&plain);
    let sealed = seal_bytes(&key, &h, &plain).unwrap();
    assert_eq!(open_bytes(&key, &h, &sealed).unwrap(), plain);
}

#[test]
fn known_answer_sealed_bytes_and_key_id_match_a_pinned_wire_format() {
    let key =
        Key::from_hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f").unwrap();
    let content_hash = "kat-fixed-content-hash-do-not-change";
    let plain = b"br8n backup known answer test".to_vec();

    let sealed = seal_bytes(&key, content_hash, &plain).unwrap();

    const EXPECTED_SEALED_HEX: &str = "42524231aa25e769709a5472b15b3cdc559c325d78c5fb644f64ede6382255b769be8ec3230318154f69b459eb98df76fa";
    const EXPECTED_KEY_ID: &str = "1309d3c5";

    assert_eq!(hex::encode(&sealed), EXPECTED_SEALED_HEX);
    assert_eq!(key.id(), EXPECTED_KEY_ID);
}
