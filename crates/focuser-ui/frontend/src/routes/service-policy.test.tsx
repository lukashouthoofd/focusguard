import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { BlockList, Command, CommandResult } from "@/bindings";
import { CommandError } from "@/lib/transport";
import { ServicePolicy } from "./service-policy";

const send = vi.hoisted(() => vi.fn());
vi.mock("@/lib/transport", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/transport")>()),
  send,
}));

const list: BlockList = {
  id: "list-1",
  name: "Deep work",
  enabled: true,
  websites: [
    { id: "domain-1", match_type: { Domain: "example.org" }, enabled: true },
    { id: "keyword-1", match_type: { Keyword: "casino" }, enabled: true },
    { id: "domain-disabled", match_type: { Domain: "inactive.example" }, enabled: false },
  ],
  applications: [{ id: "app-1", match_type: { ExecutableName: "game.exe" }, enabled: true }],
  exceptions: [{ id: "exception-1", exception_type: { Domain: "work.example" }, enabled: true }],
  lock: null,
  protection: null,
  schedule: null,
  scheduled_protection: null,
  schedule_unlocked_until: null,
  breaks: null,
  shared_allowance: null,
  created_at: "2026-11-01T00:00:00Z",
  updated_at: "2026-11-01T00:00:00Z",
};

function renderPolicy() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, refetchInterval: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <ServicePolicy />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  send.mockImplementation(async (command: Command): Promise<CommandResult> => {
    switch (command.cmd) {
      case "list_block_lists":
        return { kind: "block_lists", data: [structuredClone(list)] };
      case "get_blocking_health":
        return {
          kind: "blocking_health",
          data: {
            active_lists: 1,
            extension_connected: false,
            hosts_writable: true,
            hosts_sync_error: null,
            extension_only_rules: false,
            app_usage_measurable: false,
          },
        };
      case "add_website_rule":
        return {
          kind: "website_rule",
          data: { id: "new-rule", match_type: command.args.rule, enabled: true },
        };
      default:
        throw new CommandError({ code: "unexpected_command", message: command.cmd });
    }
  });
});

describe("Windows service policy screen", () => {
  it("shows only enabled domain rules and labels everything else as not enforced", async () => {
    renderPolicy();

    expect(await screen.findByText("example.org")).toBeInTheDocument();
    expect(screen.queryByText("casino")).not.toBeInTheDocument();
    expect(screen.queryByText("inactive.example")).not.toBeInTheDocument();
    expect(screen.getByText("Not enforced: 4")).toBeInTheDocument();
    expect(send.mock.calls.map(([command]) => (command as Command).cmd).sort()).toEqual([
      "get_blocking_health",
      "list_block_lists",
    ]);
  });

  it("sends new entries as domain-only rules to the selected list", async () => {
    renderPolicy();
    await screen.findByText("example.org");

    fireEvent.change(screen.getByRole("textbox", { name: "Value to block" }), {
      target: { value: "new.example" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));

    await waitFor(() => {
      expect(send).toHaveBeenCalledWith({
        cmd: "add_website_rule",
        args: { list_id: "list-1", rule: { Domain: "new.example" } },
      });
    });
  });
});
