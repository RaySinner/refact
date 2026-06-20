import { describe, expect, it } from "vitest";
import { taskDocumentMutationInvalidation } from "../../../services/refact/taskDocumentsApi";

describe("taskDocumentMutationInvalidation", () => {
  it("emits list tag for createTaskDocument", () => {
    expect(
      taskDocumentMutationInvalidation.createTaskDocument("task-1"),
    ).toEqual([{ type: "TaskDocuments", id: "task-1" }]);
  });

  it("emits list, detail, and history tags for updateTaskDocument", () => {
    expect(
      taskDocumentMutationInvalidation.updateTaskDocument(
        "task-1",
        "main-plan",
      ),
    ).toEqual([
      { type: "TaskDocuments", id: "task-1" },
      { type: "TaskDocuments", id: "task-1:main-plan:detail" },
      { type: "TaskDocuments", id: "task-1:main-plan:history" },
    ]);
  });

<<<<<<< HEAD
  it("emits list and detail tags for pinTaskDocument", () => {
=======
  it("emits list, detail, and history tags for pinTaskDocument", () => {
>>>>>>> upstream/main
    expect(
      taskDocumentMutationInvalidation.pinTaskDocument("task-1", "main-plan"),
    ).toEqual([
      { type: "TaskDocuments", id: "task-1" },
      { type: "TaskDocuments", id: "task-1:main-plan:detail" },
<<<<<<< HEAD
=======
      { type: "TaskDocuments", id: "task-1:main-plan:history" },
>>>>>>> upstream/main
    ]);
  });

  it("emits list, detail, and history tags for deleteTaskDocument", () => {
    expect(
      taskDocumentMutationInvalidation.deleteTaskDocument(
        "task-1",
        "main-plan",
      ),
    ).toEqual([
      { type: "TaskDocuments", id: "task-1" },
      { type: "TaskDocuments", id: "task-1:main-plan:detail" },
      { type: "TaskDocuments", id: "task-1:main-plan:history" },
    ]);
  });

  it("emits list, detail, and history tags for appendTaskDocument", () => {
    expect(
      taskDocumentMutationInvalidation.appendTaskDocument(
        "task-1",
        "main-plan",
      ),
    ).toEqual([
      { type: "TaskDocuments", id: "task-1" },
      { type: "TaskDocuments", id: "task-1:main-plan:detail" },
      { type: "TaskDocuments", id: "task-1:main-plan:history" },
    ]);
  });
});
