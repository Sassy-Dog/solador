import SwiftUI

struct ServiceCard: View {
    let service: ServiceConnection
    @State private var isHovered = false
    
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Image(systemName: service.service.icon)
                    .foregroundColor(Color(service.service.color))
                    .font(.title2)
                
                VStack(alignment: .leading, spacing: 2) {
                    Text(service.service.rawValue)
                        .font(.headline)
                    
                    if let accountName = service.accountName {
                        Text(accountName)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                
                Spacer()
                
                // Connection Status
                Circle()
                    .fill(service.isValid ? Color.green : Color.red)
                    .frame(width: 8, height: 8)
            }
            
            // Connection Info
            VStack(alignment: .leading, spacing: 4) {
                Label(
                    service.isValid ? "Connected" : "Connection Error",
                    systemImage: service.isValid ? "checkmark.circle" : "exclamationmark.circle"
                )
                .font(.caption)
                .foregroundColor(service.isValid ? .green : .red)
                
                if let lastValidated = service.lastValidated {
                    Text("Validated \(lastValidated.formatted(.relative(presentation: .named)))")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding()
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(NSColor.controlBackgroundColor))
        .cornerRadius(8)
        .shadow(color: Color.black.opacity(isHovered ? 0.15 : 0.05), radius: isHovered ? 8 : 4)
        .scaleEffect(isHovered ? 1.02 : 1.0)
        .onHover { hovering in
            withAnimation(.easeInOut(duration: 0.2)) {
                isHovered = hovering
            }
        }
        .onTapGesture {
            openServiceDashboard()
        }
        .contextMenu {
            Button("Open Dashboard") {
                openServiceDashboard()
            }
            
            Button("Refresh") {
                // Refresh connection
            }
            
            Divider()
            
            Button("Disconnect") {
                // Disconnect service
            }
        }
    }
    
    private func openServiceDashboard() {
        // Open service-specific dashboard in browser
        let urlString: String
        switch service.service {
        case .github:
            urlString = "https://github.com"
        case .vercel:
            urlString = "https://vercel.com/dashboard"
        case .neon:
            urlString = "https://console.neon.tech"
        case .cloudflare:
            urlString = "https://dash.cloudflare.com"
        }
        
        if let url = URL(string: urlString) {
            NSWorkspace.shared.open(url)
        }
    }
}