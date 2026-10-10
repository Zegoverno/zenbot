    #[test]
    fn web_caches_evict_the_oldest_entry_at_the_limit() {
        let mut cache = HashMap::new();
        let now = Instant::now();
        for i in 0..CACHE_ENTRIES {
            cache_insert(&mut cache, i, (now + Duration::from_secs(i as u64), i), |entry| entry.0);
        }
        cache_insert(&mut cache, CACHE_ENTRIES, (now + Duration::from_secs(100), 100), |entry| entry.0);
        assert_eq!(cache.len(), CACHE_ENTRIES);
        assert!(!cache.contains_key(&0));
        assert!(cache.contains_key(&CACHE_ENTRIES));
        cache_insert(&mut cache, 1, (now + Duration::from_secs(101), 101), |entry| entry.0);
        assert_eq!(cache.len(), CACHE_ENTRIES, "updating a key does not evict another entry");
    }
