use anyhow::{anyhow, Result};
use std::path::Path;

pub mod drive;
pub mod s3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteObject {
    pub key: String,
    pub size: u64,
}

pub trait Remote: Send + Sync {
    fn name(&self) -> &str;
    fn put_file(&self, key: &str, path: &Path) -> Result<()>;
    fn put_bytes(&self, key: &str, bytes: &[u8]) -> Result<()>;
    fn get_file(&self, key: &str, dest: &Path) -> Result<()>;
    fn get_bytes(&self, key: &str) -> Result<Vec<u8>>;
    fn list(&self, prefix: &str) -> Result<Vec<RemoteObject>>;
    fn delete(&self, key: &str) -> Result<()>;
}

pub struct FakeRemote {
    objects: std::sync::Mutex<std::collections::BTreeMap<String, Vec<u8>>>,
    puts: std::sync::atomic::AtomicUsize,
    fail_list_after: std::sync::Mutex<Option<usize>>,
    lists: std::sync::atomic::AtomicUsize,
    fail_put_after: std::sync::Mutex<Option<usize>>,
    fail_only_list: std::sync::Mutex<Option<usize>>,
    fail_delete_after: std::sync::Mutex<Option<usize>>,
    fail_only_delete: std::sync::Mutex<Option<usize>>,
    deletes: std::sync::atomic::AtomicUsize,
    list_reversed: std::sync::atomic::AtomicBool,
}

impl Default for FakeRemote {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeRemote {
    pub fn new() -> FakeRemote {
        FakeRemote {
            objects: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            puts: std::sync::atomic::AtomicUsize::new(0),
            fail_list_after: std::sync::Mutex::new(None),
            lists: std::sync::atomic::AtomicUsize::new(0),
            fail_put_after: std::sync::Mutex::new(None),
            fail_only_list: std::sync::Mutex::new(None),
            fail_delete_after: std::sync::Mutex::new(None),
            fail_only_delete: std::sync::Mutex::new(None),
            deletes: std::sync::atomic::AtomicUsize::new(0),
            list_reversed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn fail_list_after(&self, n: usize) {
        *self.fail_list_after.lock().unwrap() = Some(n);
    }

    pub fn fail_put_after(&self, n: usize) {
        *self.fail_put_after.lock().unwrap() = Some(n);
    }

    pub fn fail_only_list_number(&self, n: usize) {
        *self.fail_only_list.lock().unwrap() = Some(n);
    }

    pub fn fail_delete_after(&self, n: usize) {
        *self.fail_delete_after.lock().unwrap() = Some(n);
    }

    pub fn fail_only_delete_number(&self, n: usize) {
        *self.fail_only_delete.lock().unwrap() = Some(n);
    }

    pub fn list_in_reverse_key_order(&self) {
        self.list_reversed
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn keys(&self) -> Vec<String> {
        self.objects.lock().unwrap().keys().cloned().collect()
    }

    pub fn put_count(&self) -> usize {
        self.puts.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Remote for FakeRemote {
    fn name(&self) -> &str {
        "fake"
    }

    fn put_file(&self, key: &str, path: &Path) -> Result<()> {
        self.put_bytes(key, &std::fs::read(path)?)
    }

    fn put_bytes(&self, key: &str, bytes: &[u8]) -> Result<()> {
        let n = self.puts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(limit) = *self.fail_put_after.lock().unwrap() {
            if n >= limit {
                return Err(anyhow!("injected put failure"));
            }
        }
        self.objects
            .lock()
            .unwrap()
            .insert(key.to_string(), bytes.to_vec());
        Ok(())
    }

    fn get_file(&self, key: &str, dest: &Path) -> Result<()> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(dest, self.get_bytes(key)?)?;
        Ok(())
    }

    fn get_bytes(&self, key: &str) -> Result<Vec<u8>> {
        self.objects
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or_else(|| anyhow!("no such object: {key}"))
    }

    fn list(&self, prefix: &str) -> Result<Vec<RemoteObject>> {
        let n = self.lists.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(limit) = *self.fail_list_after.lock().unwrap() {
            if n >= limit {
                return Err(anyhow!("injected list failure"));
            }
        }
        if *self.fail_only_list.lock().unwrap() == Some(n) {
            return Err(anyhow!("injected one-off list failure"));
        }
        let mut listed: Vec<RemoteObject> = self
            .objects
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| RemoteObject {
                key: k.clone(),
                size: v.len() as u64,
            })
            .collect();
        if self.list_reversed.load(std::sync::atomic::Ordering::SeqCst) {
            listed.reverse();
        }
        Ok(listed)
    }

    fn delete(&self, key: &str) -> Result<()> {
        let n = self
            .deletes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(limit) = *self.fail_delete_after.lock().unwrap() {
            if n >= limit {
                return Err(anyhow!("injected delete failure"));
            }
        }
        if *self.fail_only_delete.lock().unwrap() == Some(n) {
            return Err(anyhow!("injected one-off delete failure"));
        }
        self.objects.lock().unwrap().remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fake_stores_and_returns_bytes() {
        let r = FakeRemote::new();
        r.put_bytes("blobs/abc", b"hello").unwrap();
        assert_eq!(r.get_bytes("blobs/abc").unwrap(), b"hello");
    }

    #[test]
    fn list_filters_by_prefix() {
        let r = FakeRemote::new();
        r.put_bytes("blobs/a", b"1").unwrap();
        r.put_bytes("blobs/b", b"22").unwrap();
        r.put_bytes("manifest/x.json", b"{}").unwrap();
        r.put_bytes("other/has-blobs/-in-the-middle", b"333")
            .unwrap();
        let mut keys: Vec<String> = r
            .list("blobs/")
            .unwrap()
            .into_iter()
            .map(|o| o.key)
            .collect();
        keys.sort();
        assert_eq!(keys, vec!["blobs/a", "blobs/b"]);
        assert_eq!(r.list("blobs/").unwrap()[0].size, 1);
    }

    #[test]
    fn list_excludes_a_key_with_the_prefix_only_in_the_middle() {
        let r = FakeRemote::new();
        r.put_bytes("blobs/a", b"1").unwrap();
        r.put_bytes("other/has-blobs/-in-the-middle", b"333")
            .unwrap();
        let keys: Vec<String> = r
            .list("blobs/")
            .unwrap()
            .into_iter()
            .map(|o| o.key)
            .collect();
        assert!(!keys.contains(&"other/has-blobs/-in-the-middle".to_string()));
    }

    #[test]
    fn a_missing_key_is_an_error_not_an_empty_result() {
        let r = FakeRemote::new();
        assert!(r.get_bytes("nope").is_err());
    }

    #[test]
    fn files_round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in");
        let dst = dir.path().join("out");
        std::fs::write(&src, b"payload").unwrap();
        let r = FakeRemote::new();
        r.put_file("blobs/f", &src).unwrap();
        r.get_file("blobs/f", &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"payload");
    }

    #[test]
    fn injected_list_failure_surfaces_as_an_error() {
        let r = FakeRemote::new();
        r.put_bytes("blobs/a", b"1").unwrap();
        r.fail_list_after(0);
        assert!(r.list("blobs/").is_err());
    }

    #[test]
    fn fail_list_after_one_lets_the_first_call_succeed_and_the_second_fail() {
        let r = FakeRemote::new();
        r.put_bytes("blobs/a", b"1").unwrap();
        r.fail_list_after(1);
        assert!(r.list("blobs/").is_ok());
        assert!(r.list("blobs/").is_err());
    }

    #[test]
    fn fail_list_after_two_lets_two_calls_succeed_and_the_third_fail() {
        let r = FakeRemote::new();
        r.put_bytes("blobs/a", b"1").unwrap();
        r.fail_list_after(2);
        assert!(r.list("blobs/").is_ok());
        assert!(r.list("blobs/").is_ok());
        assert!(r.list("blobs/").is_err());
    }

    #[test]
    fn injected_put_failure_surfaces_as_an_error() {
        let r = FakeRemote::new();
        r.fail_put_after(0);
        assert!(r.put_bytes("blobs/a", b"1").is_err());
    }

    #[test]
    fn fail_put_after_one_lets_the_first_call_succeed_and_the_second_fail() {
        let r = FakeRemote::new();
        r.fail_put_after(1);
        assert!(r.put_bytes("blobs/a", b"1").is_ok());
        assert!(r.put_bytes("blobs/b", b"2").is_err());
    }

    #[test]
    fn fail_delete_after_one_lets_the_first_call_succeed_and_the_second_fail() {
        let r = FakeRemote::new();
        r.put_bytes("blobs/a", b"1").unwrap();
        r.put_bytes("blobs/b", b"2").unwrap();
        r.fail_delete_after(1);
        assert!(r.delete("blobs/a").is_ok());
        assert!(r.delete("blobs/b").is_err());
        assert_eq!(r.keys(), vec!["blobs/b".to_string()]);
    }

    #[test]
    fn fail_only_list_number_one_fails_the_second_call_alone() {
        let r = FakeRemote::new();
        r.put_bytes("blobs/a", b"1").unwrap();
        r.fail_only_list_number(1);
        assert!(r.list("blobs/").is_ok());
        assert!(r.list("blobs/").is_err());
        assert!(r.list("blobs/").is_ok());
    }

    #[test]
    fn fail_only_delete_number_one_fails_the_second_call_alone() {
        let r = FakeRemote::new();
        for k in ["blobs/a", "blobs/b", "blobs/c"] {
            r.put_bytes(k, b"x").unwrap();
        }
        r.fail_only_delete_number(1);
        assert!(r.delete("blobs/a").is_ok());
        assert!(r.delete("blobs/b").is_err());
        assert!(r.delete("blobs/c").is_ok());
        assert_eq!(r.keys(), vec!["blobs/b".to_string()]);
    }

    #[test]
    fn a_reversed_listing_returns_the_same_objects_in_descending_key_order() {
        let r = FakeRemote::new();
        for k in ["blobs/a", "blobs/c", "blobs/b"] {
            r.put_bytes(k, b"x").unwrap();
        }
        r.list_in_reverse_key_order();
        let keys: Vec<String> = r
            .list("blobs/")
            .unwrap()
            .into_iter()
            .map(|o| o.key)
            .collect();
        assert_eq!(keys, vec!["blobs/c", "blobs/b", "blobs/a"]);
    }

    #[test]
    fn delete_removes_the_key() {
        let r = FakeRemote::new();
        r.put_bytes("blobs/a", b"1").unwrap();
        r.delete("blobs/a").unwrap();
        assert!(r.list("blobs/").unwrap().is_empty());
    }

    #[test]
    fn put_count_counts_calls_not_distinct_keys_and_overwrite_replaces_the_value() {
        let r = FakeRemote::new();
        r.put_bytes("blobs/a", b"1").unwrap();
        r.put_bytes("blobs/b", b"22").unwrap();
        r.put_bytes("blobs/a", b"111").unwrap();
        assert_eq!(r.put_count(), 3);
        let mut keys = r.keys();
        keys.sort();
        assert_eq!(keys, vec!["blobs/a".to_string(), "blobs/b".to_string()]);
        assert_eq!(r.get_bytes("blobs/a").unwrap(), b"111");
    }
}
