import SwiftUI
import SwiftData

struct ServicesView: View {
    @Query private var services: [ServiceConnection]
    @State private var showConnectService = false
    
    var body: some View {
        VStack {
            // Header
            HStack {
                Text("Services")
                    .font(.largeTitle)
                    .bold()
                
                Spacer()
                
                Button(action: { showConnectService = true }) {
                    Label("Connect Service", systemImage: "plus")
                }
            }
            .padding()
            
            // Services List
            if services.isEmpty {
                ContentUnavailableView {
                    Label("No Services Connected", systemImage: "cloud")
                } description: {
                    Text("Connect services to monitor your cloud infrastructure")
                } actions: {
                    Button("Connect Service") {
                        showConnectService = true
                    }
                    .buttonStyle(.borderedProminent)
                }
            } else {
                ScrollView {
                    LazyVGrid(columns: [
                        GridItem(.adaptive(minimum: 300, maximum: 400), spacing: 16)
                    ], spacing: 16) {
                        ForEach(services) { service in
                            ServiceDetailCard(service: service)
                        }
                    }
                    .padding()
                }
            }
        }
        .sheet(isPresented: $showConnectService) {
            ConnectServiceSheet()
        }
    }
}

struct ServiceDetailCard: View {
    let service: ServiceConnection
    
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            // Header
            HStack {
                Image(systemName: service.service.icon)
                    .font(.largeTitle)
                    .foregroundColor(Color(service.service.color))
                
                VStack(alignment: .leading) {
                    Text(service.service.rawValue)
                        .font(.title2)
                        .bold()
                    
                    if let accountName = service.accountName {
                        Text(accountName)
                            .foregroundStyle(.secondary)
                    }
                }
                
                Spacer()
                
                Menu {
                    Button("Open Dashboard") {
                        // Open service dashboard
                    }
                    
                    Button("Refresh") {
                        // Refresh service
                    }
                    
                    Divider()
                    
                    Button("Disconnect", role: .destructive) {
                        // Disconnect service
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
                .menuStyle(.borderlessButton)
            }
            
            Divider()
            
            // Connection Info
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Label("Status", systemImage: "circle.fill")
                        .foregroundColor(service.isValid ? .green : .red)
                    Spacer()
                    Text(service.isValid ? "Connected" : "Error")
                        .foregroundStyle(service.isValid ? .green : .red)
                }
                
                HStack {
                    Label("Connected", systemImage: "calendar")
                    Spacer()
                    Text(service.connectedAt.formatted())
                        .foregroundStyle(.secondary)
                }
                
                if let lastValidated = service.lastValidated {
                    HStack {
                        Label("Last Validated", systemImage: "checkmark.circle")
                        Spacer()
                        Text(lastValidated.formatted(.relative(presentation: .named)))
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .font(.callout)
        }
        .padding()
        .background(Color(NSColor.controlBackgroundColor))
        .cornerRadius(12)
    }
}

struct ConnectServiceSheet: View {
    @Environment(\.dismiss) private var dismiss
    @State private var selectedService: ServiceType?
    
    var body: some View {
        VStack(spacing: 20) {
            Text("Connect Service")
                .font(.title2)
                .bold()
            
            Text("Choose a service to connect")
                .foregroundStyle(.secondary)
            
            // Service Selection
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 150))], spacing: 16) {
                ForEach(ServiceType.allCases, id: \.self) { service in
                    ServiceSelectionCard(
                        service: service,
                        isSelected: selectedService == service,
                        action: { selectedService = service }
                    )
                }
            }
            
            Spacer()
            
            HStack {
                Button("Cancel") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
                
                Spacer()
                
                Button("Next") {
                    // Proceed to authentication
                }
                .keyboardShortcut(.defaultAction)
                .disabled(selectedService == nil)
                .buttonStyle(.borderedProminent)
            }
        }
        .padding()
        .frame(width: 500, height: 400)
    }
}

struct ServiceSelectionCard: View {
    let service: ServiceType
    let isSelected: Bool
    let action: () -> Void
    
    var body: some View {
        Button(action: action) {
            VStack(spacing: 8) {
                Image(systemName: service.icon)
                    .font(.largeTitle)
                    .foregroundColor(isSelected ? .white : Color(service.color))
                
                Text(service.rawValue)
                    .font(.headline)
                    .foregroundColor(isSelected ? .white : .primary)
            }
            .frame(maxWidth: .infinity)
            .frame(height: 100)
            .background(isSelected ? Color.accentColor : Color(NSColor.controlBackgroundColor))
            .cornerRadius(8)
            .overlay(
                RoundedRectangle(cornerRadius: 8)
                    .stroke(isSelected ? Color.clear : Color.secondary.opacity(0.3), lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
    }
}