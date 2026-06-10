import { describe, expect, test } from "vitest";
import { render, screen } from "../../utils/test-utils";
import { ErrorMessageCard } from "./ErrorMessage";
import type { ErrorMessage } from "../../services/refact/types";

describe("ErrorMessageCard", () => {
  test("renders unstructured errors as plain text instead of markdown subblocks", () => {
    const error: ErrorMessage = {
      role: "error",
      content:
        'Request failed\n- provider returned 500\n```json\n{"error":true}\n```',
    };

    const { container } = render(<ErrorMessageCard errors={[error]} />);

    expect(container).toHaveTextContent("Request failed");
    expect(container).toHaveTextContent("- provider returned 500");
    expect(container).toHaveTextContent('{"error":true}');
    expect(container.querySelector("pre")).not.toBeInTheDocument();
    expect(container.querySelector("ul")).not.toBeInTheDocument();
  });

  test("keeps a single structured error flat inside one card", () => {
    const error: ErrorMessage = {
      role: "error",
      content: "provider overloaded",
      error_info: {
        category: "ProviderTransient",
        title: "Provider is busy",
        explanation: "The provider is temporarily overloaded.",
        suggested_action: "retry",
        is_retryable: true,
        raw_error: "HTTP 503\n- overloaded",
      },
    };

    const { container } = render(<ErrorMessageCard errors={[error]} />);

    expect(screen.getByText("Provider is busy")).toBeInTheDocument();
    expect(screen.getByText("ProviderTransient")).toBeInTheDocument();
    expect(screen.getByText("Retry")).toBeInTheDocument();
    expect(container).toHaveTextContent("HTTP 503");
    expect(container).toHaveTextContent("- overloaded");
    expect(container.querySelector("pre")).not.toBeInTheDocument();
    expect(container.querySelector("ul")).not.toBeInTheDocument();
  });
});
