import Foundation

/// Localized lookup. Keys are the English text; translations live in
/// `Resources/<lang>.lproj/Localizable.strings` and are copied into the app bundle by
/// scripts/macos/build-app.sh. When a key is missing the English key itself is shown.
public func L(_ key: String) -> String {
    NSLocalizedString(key, bundle: .main, comment: "")
}

/// Localized format string, e.g. `LF("%d files", 3)`.
public func LF(_ key: String, _ args: CVarArg...) -> String {
    String(format: NSLocalizedString(key, bundle: .main, comment: ""), locale: Locale.current, arguments: args)
}

enum ByteFormat {
    static func string(_ bytes: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }
}
