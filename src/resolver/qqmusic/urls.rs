use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SongId {
    Id(u64),
    Mid(String),
}

pub(super) fn valid_mid(mid: &str) -> bool {
    (10..=32).contains(&mid.len()) && mid.bytes().all(|c| c.is_ascii_alphanumeric())
}

fn parse_id(value: &str, is_mid: bool) -> Option<SongId> {
    if !is_mid
        && let Ok(id) = value.parse::<u64>()
        && id > 0
    {
        return Some(SongId::Id(id));
    }
    valid_mid(value).then(|| SongId::Mid(value.to_owned()))
}

pub(super) fn song_id(url: &Url) -> Option<SongId> {
    for (key, value) in url.query_pairs() {
        if matches!(key.as_ref(), "songmid" | "mid") {
            return parse_id(&value, true);
        }
        if matches!(key.as_ref(), "songid" | "song_id" | "id") {
            return parse_id(&value, false);
        }
    }
    let segments = url.path_segments()?.collect::<Vec<_>>();
    for pair in segments.windows(2) {
        if matches!(pair[0], "songDetail" | "song") {
            return parse_id(pair[1].trim_end_matches(".html"), false);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_numeric_and_mid_song_routes_and_queries() {
        for input in [
            "https://y.qq.com/n/ryqq_v2/songDetail/607240405",
            "https://y.qq.com/n/m/detail/song.html?songid=607240405",
        ] {
            assert_eq!(
                song_id(&Url::parse(input).unwrap()),
                Some(SongId::Id(607240405))
            );
        }
        for input in [
            "https://y.qq.com/n/yqq/song/002d5NTY0FgLNi.html",
            "https://y.qq.com/n/m/detail/song.html?songmid=002d5NTY0FgLNi",
        ] {
            assert_eq!(
                song_id(&Url::parse(input).unwrap()),
                Some(SongId::Mid("002d5NTY0FgLNi".into()))
            );
        }
        assert!(song_id(&Url::parse("https://y.qq.com/n/ryqq/albumDetail/123").unwrap()).is_none());
        assert!(
            song_id(
                &Url::parse("https://y.qq.com/n/m/detail/song.html?songmid=bad%2Fmid").unwrap()
            )
            .is_none()
        );
    }
}
