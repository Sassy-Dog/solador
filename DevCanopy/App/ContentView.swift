import SwiftData
import SwiftUI

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
