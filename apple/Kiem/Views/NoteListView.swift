import SwiftUI
import KiemKit

struct NoteListView: View {
    @Bindable var model: KiemModel

    var body: some View {
        Group {
            if model.notes.isEmpty {
                ContentUnavailableView(
                    model.selectedTag.map { "No notes tagged #\($0)" } ?? "No notes yet",
                    systemImage: "note.text",
                    description: Text("Create a note with ⌘N.")
                )
            } else {
                List(selection: $model.selectedNoteID) {
                    ForEach(model.notes, id: \.id) { note in
                        NoteRow(note: note)
                            .tag(note.id)
                            .contextMenu {
                                Button("Move to Trash", role: .destructive) {
                                    model.deleteNote(id: note.id)
                                }
                            }
                    }
                }
            }
        }
    }
}

private struct NoteRow: View {
    let note: NoteMetadata

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(note.title.isEmpty ? "Untitled" : note.title)
                .font(.headline)
                .lineLimit(1)
            HStack(spacing: 6) {
                Text(Self.dateText(note.modifiedAt))
                    .foregroundStyle(.secondary)
                ForEach(note.tags, id: \.self) { tag in
                    Text("#\(tag)")
                        .foregroundStyle(.tint)
                }
            }
            .font(.caption)
            .lineLimit(1)
        }
        .padding(.vertical, 2)
    }

    private static func dateText(_ rfc3339: String) -> String {
        guard let date = ISO8601DateFormatter.flexible.date(from: rfc3339) else {
            return rfc3339
        }
        return date.formatted(.relative(presentation: .named))
    }
}

extension ISO8601DateFormatter {
    /// Note timestamps carry fractional seconds; tolerate both forms.
    static let flexible: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()
}
