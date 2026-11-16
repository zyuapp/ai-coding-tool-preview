//! How the panel presents itself to a site. Sites refuse sign-in from a user agent that names an
//! embedded web view, and WebKit's own leaves out the Safari version, so the panel says it is the
//! Safari this machine has: the engine it really is, so no site serves it code for another.

const FALLBACK_VERSION: &str = "18.0";

/// `Version/x.y` as Safari states it, from the major and minor of its bundle version.
pub fn safari_version(info_plist: &str) -> Option<String> {
    let key = info_plist.find("<key>CFBundleShortVersionString</key>")?;
    let rest = &info_plist[key..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")? + start;
    let version = rest[start..end].trim();
    let mut parts = version.split('.');
    let major = parts.next().filter(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()))?;
    let minor = parts.next().filter(|part| part.chars().all(|ch| ch.is_ascii_digit())).unwrap_or("0");
    Some(format!("{major}.{minor}"))
}

pub fn user_agent() -> String {
    let version = std::fs::read_to_string("/Applications/Safari.app/Contents/Info.plist")
        .ok()
        .and_then(|plist| safari_version(&plist))
        .unwrap_or_else(|| FALLBACK_VERSION.to_string());
    format!("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/{version} Safari/605.1.15")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_major_and_minor_version() {
        let plist = "<dict><key>CFBundleShortVersionString</key>\n\t<string>26.1.2</string></dict>";
        assert_eq!(safari_version(plist).as_deref(), Some("26.1"));
        assert_eq!(safari_version("<dict></dict>"), None);
    }
}
