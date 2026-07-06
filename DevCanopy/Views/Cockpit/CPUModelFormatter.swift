import Foundation

/// Cleans raw CPU brand strings for the host card header. The raw value comes from
/// sysctl `machdep.cpu.brand_string` locally and from the same `CPUMetrics.model`
/// field decoded from remote agents, so cleaning here at the view layer covers both.
/// Vendor noise — "(R)"/"(TM)", the standalone "CPU" token, AMD's trailing
/// "N-Core Processor" suffix, and the doubled spaces Xeon strings ship with — adds
/// width but no information on a glanceable card. Apple Silicon strings ("Apple M2
/// Max") contain none of these tokens and pass through unchanged.
enum CPUModelFormatter {
    static func clean(_ raw: String) -> String {
        var model = raw
            .replacingOccurrences(of: "(R)", with: "")
            .replacingOccurrences(of: "(TM)", with: "")
        model = model.replacingOccurrences(
            of: #"\s\d+-Core Processor\s*$"#,
            with: "",
            options: .regularExpression
        )
        // Splitting and rejoining collapses whitespace runs and drops "CPU" tokens.
        return model.split(separator: " ").filter { $0 != "CPU" }.joined(separator: " ")
    }
}
