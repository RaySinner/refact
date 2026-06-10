import { describe, expect, it } from "vitest";
import { render, screen } from "../../utils/test-utils";
import { TaskDocumentsContent } from "./TaskDocumentsView";

const DOC_LIST = [
  "| slug | name | kind | pinned | version | updated_at |",
  "|---|---|---|---|---:|---|",
  "| main-plan | Main Plan | plan | true | 3 | 2026-05-22T10:00:00Z |",
  "| api-spec | API Spec | spec | false | 1 | 2026-05-22T11:00:00Z |",
  "| runbook | Deploy Runbook | runbook | true | 2 | 2026-05-22T12:00:00Z |",
].join("\n");

const DOC_GET = [
  "---",
  "name: Main Plan",
  "slug: main-plan",
  "kind: plan",
  "pinned: true",
  "version: 3",
  "---",
  "",
  "# Main Plan",
  "",
  "- Ship document renderer",
].join("\n");

const UNSAFE_DOC_GET = [
  "---",
  "name: Unsafe Document",
  "slug: unsafe-document",
  "kind: plan",
  "pinned: false",
  "version: 1",
  "---",
  "",
  "# Unsafe Document",
  "",
  "<script>window.x=1</script>",
  '<img onerror="alert(1)" src=x>',
].join("\n");

describe("TaskDocumentsContent", () => {
  it("parses doc_list markdown into rows and renders them", () => {
    render(<TaskDocumentsContent toolType="doc_list" content={DOC_LIST} />);

    expect(screen.getByText("Main Plan")).toBeInTheDocument();
    expect(screen.getByText("API Spec")).toBeInTheDocument();
    expect(screen.getByText("Deploy Runbook")).toBeInTheDocument();
    expect(screen.getByText("3 documents")).toBeInTheDocument();
  });

  it("parses doc_list alias column names and skips rows without slugs", () => {
    render(
      <TaskDocumentsContent
        toolType="doc_list"
        content={[
          "| slug | title | kind | pinned | version | updated at |",
          "|---|---|---|---|---:|---|",
          "| aliased-plan | Aliased Plan | plan | yes | 4 | 2026-05-22T13:00:00Z |",
          "|  | Missing Slug | brief | true | 1 | 2026-05-22T14:00:00Z |",
        ].join("\n")}
      />,
    );

    expect(screen.getByText("Aliased Plan")).toBeInTheDocument();
    expect(screen.getByText("2026-05-22T13:00:00Z")).toBeInTheDocument();
    expect(screen.getByText("1 documents")).toBeInTheDocument();
    expect(screen.queryByText("Missing Slug")).not.toBeInTheDocument();
  });

  it("parses updated alias in doc_list markdown", () => {
    render(
      <TaskDocumentsContent
        toolType="doc_list"
        content={[
          "| slug | name | kind | pinned | version | updated |",
          "|---|---|---|---|---:|---|",
          "| updated-plan | Updated Plan | plan | no | 1 | 2026-05-22T15:00:00Z |",
        ].join("\n")}
      />,
    );

    expect(screen.getByText("Updated Plan")).toBeInTheDocument();
    expect(screen.getByText("2026-05-22T15:00:00Z")).toBeInTheDocument();
  });

  it("renders raw doc_list markdown when no rows parse", () => {
    render(
      <TaskDocumentsContent toolType="doc_list" content="No documents found" />,
    );

    expect(screen.getByText("No documents found")).toBeInTheDocument();
    expect(
      screen.getByText("Parser produced no rows; raw output below"),
    ).toBeInTheDocument();
    expect(screen.queryByText("0 documents")).not.toBeInTheDocument();
  });

  it("does not render the raw fallback when doc_list rows parse", () => {
    render(<TaskDocumentsContent toolType="doc_list" content={DOC_LIST} />);

    expect(
      screen.queryByText("Parser produced no rows; raw output below"),
    ).not.toBeInTheDocument();
  });

  it("renders doc_get body markdown", () => {
    render(<TaskDocumentsContent toolType="doc_get" content={DOC_GET} />);

    expect(
      screen.getByRole("heading", { name: "Main Plan" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Ship document renderer")).toBeInTheDocument();
    expect(screen.getByText("main-plan")).toBeInTheDocument();
    expect(screen.getByText("v3")).toBeInTheDocument();
  });

  it("does not render raw scripts or inline image handlers from doc_get markdown", () => {
    const { container } = render(
      <TaskDocumentsContent toolType="doc_get" content={UNSAFE_DOC_GET} />,
    );

    expect(container.querySelector("script")).not.toBeInTheDocument();
    expect(container.querySelector("img[onerror]")).not.toBeInTheDocument();
  });

  it("renders pinned and non-pinned stars differently", () => {
    render(<TaskDocumentsContent toolType="doc_list" content={DOC_LIST} />);

    expect(screen.getByLabelText("Pinned main-plan")).toHaveTextContent("★");
    expect(screen.getByLabelText("Not pinned api-spec")).toHaveTextContent("☆");
  });
});
