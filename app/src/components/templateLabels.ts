import type { TFunction } from "i18next";
import type { Template } from "@/lib/ipc";

// Compare metadata against its shipped value so edits to built-ins stay visible.
// Display labels never replace stored names or prompt bodies in the editor.
const BUILTINS = [
  {
    "id": "general",
    "name": "General meeting",
    "description": "Balanced notes for any meeting: summary, decisions, action items, open questions, key moments.",
    "nameKey": "builtins.general.name",
    "descriptionKey": "builtins.general.description"
  },
  {
    "id": "sales-call",
    "name": "Sales call",
    "description": "Discovery and deal notes: pain points, budget, timeline, objections, next steps.",
    "nameKey": "builtins.sales-call.name",
    "descriptionKey": "builtins.sales-call.description"
  },
  {
    "id": "client-discovery",
    "name": "Client discovery",
    "description": "Requirements gathering: goals, current state, constraints, scope, next steps.",
    "nameKey": "builtins.client-discovery.name",
    "descriptionKey": "builtins.client-discovery.description"
  },
  {
    "id": "one-on-one",
    "name": "1:1",
    "description": "Private manager/report notes: updates, wins, blockers, growth, commitments.",
    "nameKey": "builtins.one-on-one.name",
    "descriptionKey": "builtins.one-on-one.description"
  },
  {
    "id": "standup",
    "name": "Standup",
    "description": "Fast team sync: per-person updates, blockers, decisions.",
    "nameKey": "builtins.standup.name",
    "descriptionKey": "builtins.standup.description"
  },
  {
    "id": "interview",
    "name": "Interview",
    "description": "Candidate interview notes: background, evidence, strengths, concerns, recommendation inputs.",
    "nameKey": "builtins.interview.name",
    "descriptionKey": "builtins.interview.description"
  }
] as const;

export function templateName(template: Template, tr: TFunction<"meeting">): string {
  const builtin = BUILTINS.find((item) => item.id === template.id);
  return template.builtin && builtin && template.name === builtin.name
    ? tr(builtin.nameKey) : template.name;
}

export function templateDescription(template: Template, tr: TFunction<"meeting">): string {
  const builtin = BUILTINS.find((item) => item.id === template.id);
  return template.builtin && builtin && template.description === builtin.description
    ? tr(builtin.descriptionKey) : template.description;
}
