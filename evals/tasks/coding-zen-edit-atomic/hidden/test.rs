    #[tokio::test]
    async fn edit_keeps_mixed_line_endings_and_writes_through_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let ws = scratch("mixed");
        std::fs::write(ws.join("m.txt"), "a\r\nb\nc\r\n").unwrap();
        std::fs::set_permissions(ws.join("m.txt"), std::fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink(ws.join("m.txt"), ws.join("link.txt")).unwrap();
        let r = run(&ws, "edit", json!({ "path": "link.txt", "old_text": "b", "new_text": "B" })).await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(std::fs::read_to_string(ws.join("m.txt")).unwrap(), "a\r\nB\nc\r\n");
        assert!(std::fs::symlink_metadata(ws.join("link.txt")).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::metadata(ws.join("m.txt")).unwrap().permissions().mode() & 0o777, 0o640);
        let leftovers = std::fs::read_dir(&*ws).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains(".zen-")).count();
        assert_eq!(leftovers, 0);
    }
