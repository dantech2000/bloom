import BloomCore
import SwiftUI

struct TasksPage: View {
    @Environment(AppModel.self) private var model
    @State private var find = ""

    var body: some View {
        Loaded(title: "Scheduled tasks", every: 3, load: { try await model.adminServer().adminTasks() }) { tasks, reload in
            let shown = find.isEmpty ? tasks : tasks.filter { $0.name.localizedCaseInsensitiveContains(find) || $0.description.localizedCaseInsensitiveContains(find) }
            List {
                ForEach(Dictionary(grouping: shown, by: \.category).sorted { $0.key < $1.key }, id: \.key) { category, tasks in
                    Section(category) {
                        ForEach(tasks, id: \.id) { task in
                            NavigationLink { TaskPage(task: task) } label: { taskRow(task, reload: reload) }
                        }
                    }
                }
            }
            .searchable(text: $find, prompt: "Find a task")
        }
    }

    private func taskRow(_ task: AdminTask, reload: @escaping () async -> Void) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline) {
                Text(task.name)
                Spacer()
                Button(task.state == .idle ? "Run" : "Stop") {
                    Task {
                        do { try await model.server?.adminRunTask(taskId: task.id, stop: task.state != .idle); await reload() } catch {
                            model.say(Words.error(error))
                        }
                    }
                }
                .buttonStyle(.borderless)
            }
            if task.state != .idle {
                ProgressView(value: (task.progress ?? 0) / 100)
            } else if let result = task.lastResult {
                if result == .failed {
                    StateLine(text: "\(result.name()) \(When.text(task.lastEnd))", color: .fail).font(.caption).foregroundStyle(.secondary)
                } else {
                    Text("\(result.name()) \(When.text(task.lastEnd))").font(.caption).foregroundStyle(.secondary)
                }
            }
            Text(task.schedule).font(.caption).foregroundStyle(.secondary)
            Text(task.description).font(.caption).foregroundStyle(.secondary).lineLimit(3)
        }
    }
}

/// One scheduled task: Run or Stop, and its triggers (the Mac's triggers
/// editor). Each change reads the triggers again, changes that list and
/// sends it back whole.
struct TaskPage: View {
    @Environment(AppModel.self) private var model
    let task: AdminTask
    @State private var triggers: [TaskTrigger]?
    @State private var error: String?
    @State private var busy = false
    @State private var kind = TriggerKind.daily
    @State private var time = Calendar.current.date(bySettingHour: 3, minute: 0, second: 0, of: .now) ?? .now
    @State private var day: UInt8 = 0
    @State private var hours = 12
    @State private var limited = false
    @State private var limitHours = 2

    var body: some View {
        List {
            if !task.description.isEmpty {
                Section { Text(task.description).foregroundStyle(.secondary) }
            }
            Section {
                if let triggers {
                    ForEach(triggers, id: \.raw) { trigger in
                        Text(trigger.text)
                            .swipeActions {
                                Button("Remove", role: .destructive) { change("Trigger removed") { try await $0.adminRemoveTaskTrigger(taskId: task.id, raw: trigger.raw) } }
                            }
                    }
                    if triggers.isEmpty { Text("Manual only").foregroundStyle(.secondary) }
                } else if let error {
                    Text(error).foregroundStyle(Color.fail)
                } else {
                    ProgressView()
                }
            } header: {
                Text("Triggers")
            } footer: {
                Text("Times are the server's local time. Swipe a trigger to remove it.")
            }
            Section("Add a trigger") {
                Picker("Kind", selection: $kind) {
                    Text("Daily").tag(TriggerKind.daily)
                    Text("Weekly").tag(TriggerKind.weekly)
                    Text("Interval").tag(TriggerKind.interval)
                    Text("Startup").tag(TriggerKind.startup)
                }
                if kind == .weekly {
                    Picker("Day", selection: $day) {
                        ForEach(Array(Calendar(identifier: .gregorian).weekdaySymbols.enumerated()), id: \.offset) { n, name in
                            Text(name).tag(UInt8(n))
                        }
                    }
                }
                if kind == .daily || kind == .weekly {
                    DatePicker("Time", selection: $time, displayedComponents: .hourAndMinute)
                }
                if kind == .interval {
                    Stepper("Every \(hours) \(hours == 1 ? "hour" : "hours")", value: $hours, in: 1...168)
                }
                Toggle("Time limit", isOn: $limited)
                if limited {
                    Stepper("Stops after \(limitHours) \(limitHours == 1 ? "hour" : "hours")", value: $limitHours, in: 1...24)
                }
                Button("Add trigger") {
                    let parts = Calendar.current.dateComponents([.hour, .minute], from: time)
                    let minutes = Int64((parts.hour ?? 0) * 60 + (parts.minute ?? 0))
                    let (kind, day, interval, limit) = (kind, day, Int64(hours * 60), limited ? Int64(limitHours * 60) : nil)
                    change("Trigger added") {
                        try await $0.adminAddTaskTrigger(taskId: task.id, kind: kind, minutes: minutes, day: day, intervalMinutes: interval, limitMinutes: limit)
                    }
                }
                .disabled(busy || triggers == nil)
            }
        }
        .navigationTitle(task.name)
        .navigationBarTitleDisplayMode(.inline)
        .task {
            do { triggers = try await model.adminServer().adminTaskTriggers(taskId: task.id) } catch { self.error = Words.error(error) }
        }
    }

    private func change(_ done: String, _ work: @escaping (Server) async throws -> [TaskTrigger]) {
        guard !busy, let server = model.server else { return }
        busy = true
        Task {
            defer { busy = false }
            do {
                triggers = try await work(server)
                model.say(done)
            } catch {
                model.say(Words.error(error))
            }
        }
    }
}
