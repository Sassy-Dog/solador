import SwiftUI
import SwiftData

struct ContentView: View {
    var body: some View {
        CockpitView()
            .preferredColorScheme(.dark)
    }
}

#Preview {
    ContentView()
        .modelContainer(for: MonitoredHost.self, inMemory: true)
}