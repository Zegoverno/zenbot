    #[test]
    fn legacy_pages_decode_from_their_charset() {
        let latin1 = b"<p>S\xe3o Paulo \x96 cora\xe7\xe3o</p>";
        assert_eq!(decode_page(latin1, "text/html; charset=ISO-8859-1"), "<p>São Paulo – coração</p>");
        assert_eq!(decode_page(latin1, "text/html"), "<p>São Paulo – coração</p>", "invalid UTF-8, nothing declared");
        let meta = b"<meta charset=\"windows-1252\"><p>\x93ok\x94</p>";
        assert_eq!(decode_page(meta, "text/html"), "<meta charset=\"windows-1252\"><p>“ok”</p>");
        assert_eq!(decode_page("São".as_bytes(), "text/html; charset=utf-8"), "São");
        assert_eq!(decode_page("São".as_bytes(), ""), "São");
    }
