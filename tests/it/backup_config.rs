use br8n::config::Config;

#[test]
fn an_absent_backup_table_means_not_configured() {
    let c = Config::default();
    assert!(!c.backup.enabled, "backup must be opt-in");
    assert!(c.backup.targets.is_empty());
}

#[test]
fn a_partial_backup_table_keeps_every_other_default() {
    let c: Config = toml::from_str("[backup]\nenabled = true\n").unwrap();
    assert!(c.backup.enabled);
    assert!(c.backup.encrypt, "encryption must stay on");
    assert!(c.backup.include_index);
    assert_eq!(c.backup.keep_generations, 30);
    assert_eq!(c.backup.keep_index, 2);
}

#[test]
fn s3_and_drive_subtables_parse_with_defaults() {
    let c: Config = toml::from_str(
        r#"
        [backup]
        enabled = true
        targets = ["s3", "drive"]

        [backup.s3]
        bucket = "my-br8n-backups"
        region = "eu-west-1"

        [backup.drive]
        folder_id = "1AbC"
        client_secret_file = "/tmp/drive-client.json"
        "#,
    )
    .unwrap();
    assert_eq!(
        c.backup.targets,
        vec!["s3".to_string(), "drive".to_string()]
    );
    let s3 = c.backup.s3.as_ref().unwrap();
    assert_eq!(s3.bucket, "my-br8n-backups");
    assert_eq!(s3.prefix, "br8n/");
    assert_eq!(s3.profile, "default");
    assert_eq!(s3.storage_class, "STANDARD_IA");
    let d = c.backup.drive.as_ref().unwrap();
    assert_eq!(d.folder_id, "1AbC");
    assert!(d.token_file.is_none(), "resolved lazily, not at parse time");
}

#[test]
fn key_and_token_paths_sit_beside_the_database() {
    let cfg = Config::default();
    assert!(cfg.backup_key_path().ends_with("backup.key"));
    assert!(cfg.drive_token_path().ends_with("drive-token.json"));
    assert!(Config::backup_log_path().ends_with("db.backup.log"));
}

#[test]
fn a_drive_table_without_a_folder_id_parses_so_auth_can_create_one() {
    let cfg: br8n::config::Config = toml::from_str(
        "[backup]\nenabled = true\ntargets = [\"drive\"]\n[backup.drive]\nclient_secret_file = \"~/c.json\"\n",
    )
    .unwrap();
    assert_eq!(cfg.backup.drive.unwrap().folder_id, "");
}
