import SwiftUI

/// The width the cockpit has allotted to the panel currently being rendered.
///
/// Panels don't measure themselves. `CockpitView` reads the window width once from a
/// single `GeometryReader` and, because it knows each row's composition after
/// `CockpitBreakpoints.reflow`, it can *derive* every panel's width exactly — a
/// `.full` panel gets the content width, two `.half` panels split it minus the gap.
///
/// Deriving beats measuring here on two counts: there's no measure → relayout →
/// measure feedback loop, and no dependence on preferences propagating out of a
/// `.background(GeometryReader { … })`, which this SwiftUI version does not deliver
/// (it silently leaves the reader at its `defaultValue`, which is how the Hosts panel
/// spent its life believing every window was wide enough).
///
/// Defaults to 0, meaning "unknown" — panels rendered outside the cockpit (previews)
/// see that and fall back to their roomiest layout.
extension EnvironmentValues {
    @Entry var cockpitPanelWidth: CGFloat = 0
}
