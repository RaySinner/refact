import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Theme } from "@radix-ui/themes";
import { describe, expect, it } from "vitest";
import { MessageFooter } from "./MessageFooter";

function renderFooter() {
  return render(
    <Theme>
      <MessageFooter
        usage={{
          prompt_tokens: 3,
          completion_tokens: 11,
          cache_read_input_tokens: 10_600,
          cache_creation_input_tokens: 5_400,
          total_tokens: 16_014,
        }}
      />
    </Theme>,
  );
}

function renderFooterWithAliases() {
  return render(
    <Theme>
      <MessageFooter
        usage={{
          prompt_tokens: 3,
          completion_tokens: 11,
          cache_read_tokens: 10_600,
          cache_creation_tokens: 5_400,
          total_tokens: 16_014,
        }}
      />
    </Theme>,
  );
}

describe("MessageFooter", () => {
  it("shows Anthropic cache usage token details", async () => {
    renderFooter();

    await userEvent.hover(screen.getByText("16.00k"));

    expect(await screen.findByText("Context size")).toBeInTheDocument();
    expect(screen.getByText("Cache read")).toBeInTheDocument();
    expect(screen.getByText("10.60k")).toBeInTheDocument();
    expect(screen.getByText("Cache creation")).toBeInTheDocument();
    expect(screen.getByText("5.40k")).toBeInTheDocument();
  });

  it("shows cache usage token details from legacy aliases", async () => {
    renderFooterWithAliases();

    await userEvent.hover(screen.getByText("16.00k"));

    expect(await screen.findByText("Context size")).toBeInTheDocument();
    expect(screen.getByText("Cache read")).toBeInTheDocument();
    expect(screen.getByText("10.60k")).toBeInTheDocument();
    expect(screen.getByText("Cache creation")).toBeInTheDocument();
    expect(screen.getByText("5.40k")).toBeInTheDocument();
  });
});
