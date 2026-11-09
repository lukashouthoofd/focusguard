import { Globe, ListChecks, Plus, ShieldCheck, Trash2 } from "lucide-react";
import { type FormEvent, useMemo, useState } from "react";
import type { BlockList, WebsiteRule } from "@/bindings";
import { RuleTable } from "@/components/rule-table";
import { ScheduleGrid } from "@/components/schedule-grid";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, EmptyState, PageHeader } from "@/components/ui/card";
import { ConfirmButton } from "@/components/ui/confirm-button";
import { InlineError, QueryState } from "@/components/ui/feedback";
import { Input } from "@/components/ui/input";
import { Page } from "@/components/ui/page";
import { Switch } from "@/components/ui/switch";
import { Tabs } from "@/components/ui/tabs";
import {
  useAddWebsiteRule,
  useBlockingHealth,
  useBlockLists,
  useCreateBlockList,
  useDeleteBlockList,
  useRemoveWebsiteRule,
  useToggleBlockList,
  useUpdateSchedule,
} from "@/lib/commands";
import { describeWebsite } from "@/lib/match-types";
import { type CellKey, cellsToSlots, DAYS, HOURS, slotsToCells } from "@/lib/schedule";
import { m } from "@/paraglide/messages.js";

const WEEK_HOURS = DAYS.length * HOURS.length;

function isDomainRule(rule: WebsiteRule): rule is WebsiteRule & { match_type: { Domain: string } } {
  return typeof rule.match_type === "object" && "Domain" in rule.match_type;
}

function isExecutableNameRule(rule: BlockList["applications"][number]) {
  return typeof rule.match_type === "object" && "ExecutableName" in rule.match_type;
}

/** The deliberately small policy surface supported by the Windows service. */
export function ServicePolicy() {
  const lists = useBlockLists();
  const health = useBlockingHealth();
  const create = useCreateBlockList();
  const [name, setName] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [panel, setPanel] = useState<"websites" | "schedule">("websites");

  const selected = lists.data?.find((list) => list.id === selectedId) ?? lists.data?.[0] ?? null;

  function onCreate(event: FormEvent) {
    event.preventDefault();
    const trimmed = name.trim();
    if (!trimmed) return;
    create.mutate(trimmed, {
      onSuccess: (list) => {
        setName("");
        setSelectedId(list.id);
      },
    });
  }

  return (
    <Page wide>
      <PageHeader title={m.service_title()} description={m.service_description()} />

      <Card className="mb-6 border-warning/30 bg-warning/5" padding="md" elevation="raised">
        <p className="flex items-start gap-2 text-sm text-muted-foreground">
          <ShieldCheck aria-hidden className="mt-0.5 size-4 shrink-0 text-warning" />
          <span>{m.service_scope_notice()}</span>
        </p>
      </Card>

      <div className="grid gap-6 xl:grid-cols-[minmax(18rem,0.8fr)_minmax(28rem,1.4fr)]">
        <section>
          <SectionHeading title={m.service_lists_title()} />
          <Card className="mb-4" padding="md" elevation="raised">
            <form onSubmit={onCreate} className="flex gap-2">
              <Input
                value={name}
                onChange={(event) => setName(event.target.value)}
                placeholder={m.lists_new_placeholder()}
                aria-label={m.lists_new_label()}
              />
              <Button type="submit" icon={<Plus />} disabled={!name.trim() || create.isPending}>
                {m.lists_create()}
              </Button>
            </form>
            <InlineError error={create.error} />
          </Card>

          <QueryState
            isPending={lists.isPending}
            error={lists.error}
            onRetry={() => void lists.refetch()}
            isRetrying={lists.isFetching}
          >
            {lists.data?.length === 0 ? (
              <EmptyState
                icon={<ListChecks />}
                title={m.lists_empty_title()}
                description={m.lists_empty_description()}
              />
            ) : (
              <ul className="flex flex-col gap-2">
                {lists.data?.map((list) => (
                  <ServiceListRow
                    key={list.id}
                    list={list}
                    selected={selected?.id === list.id}
                    onSelect={() => setSelectedId(list.id)}
                  />
                ))}
              </ul>
            )}
          </QueryState>
        </section>

        <section className="min-w-0">
          {selected ? (
            <>
              <Tabs
                className="mb-4"
                value={panel}
                onChange={setPanel}
                items={[
                  { id: "websites", label: m.websites_title() },
                  { id: "schedule", label: m.schedule_title() },
                ]}
              />
              {panel === "websites" ? (
                <DomainRules list={selected} />
              ) : (
                <ServiceSchedule key={selected.id} list={selected} />
              )}
            </>
          ) : (
            <>
              <SectionHeading title={m.websites_title()} />
              <EmptyState icon={<Globe />} title={m.service_select_list()} />
            </>
          )}

          <ServiceHealth health={health} />
        </section>
      </div>
    </Page>
  );
}

function sameCells(left: Set<CellKey>, right: Set<CellKey>) {
  return left.size === right.size && [...left].every((cell) => right.has(cell));
}

function ServiceSchedule({ list }: { list: BlockList }) {
  const save = useUpdateSchedule();
  const saved = useMemo(() => slotsToCells(list.schedule?.time_slots ?? []), [list.schedule]);
  const alwaysActive = list.schedule === null;
  const [draft, setDraft] = useState<Set<CellKey>>(() => saved);
  const [mode, setMode] = useState<"always" | "scheduled">(() =>
    alwaysActive ? "always" : "scheduled",
  );

  const dirty = mode === "always" ? !alwaysActive : alwaysActive || !sameCells(draft, saved);
  const hasAllowanceOrScheduledLock =
    list.scheduled_protection !== null || list.shared_allowance !== null;
  const leavesGap = mode === "scheduled" && draft.size > 0 && draft.size < WEEK_HOURS;
  const emptySchedule = mode === "scheduled" && draft.size === 0;
  const needsGap = hasAllowanceOrScheduledLock && !leavesGap && !emptySchedule;
  const saveDisabled = !dirty || save.isPending || needsGap || emptySchedule;

  function commit() {
    if (!dirty || needsGap || emptySchedule) return;
    save.mutate({
      listId: list.id,
      slots: mode === "always" ? [] : cellsToSlots(draft),
      alwaysActive: mode === "always",
    });
  }

  return (
    <>
      <SectionHeading title={m.schedule_title()} />
      <Card className="mb-4" padding="md" elevation="raised">
        <div className="flex flex-col gap-4">
          <Tabs
            value={mode}
            onChange={setMode}
            items={[
              { id: "always", label: m.schedule_mode_always() },
              { id: "scheduled", label: m.schedule_mode_scheduled() },
            ]}
          />
          {mode === "scheduled" ? (
            <div>
              <p className="mb-3 text-muted-foreground text-xs">{m.grid_hint()}</p>
              <ScheduleGrid selected={draft} onChange={setDraft} disabled={save.isPending} />
              {emptySchedule && (
                <p className="mt-2 text-warning text-xs">{m.schedule_empty_selection()}</p>
              )}
            </div>
          ) : (
            <p className="text-muted-foreground text-sm">{m.schedule_always_heading()}</p>
          )}
          {needsGap && (
            <p role="alert" className="text-warning text-xs">
              {m.schedule_needs_gap_body({ name: list.name })}
            </p>
          )}
          <div className="flex items-center justify-between gap-3">
            <span className="text-faint-foreground text-xs">
              {dirty ? m.schedule_unsaved() : m.schedule_saved()}
            </span>
            <Button onClick={commit} disabled={saveDisabled}>
              {m.schedule_save()}
            </Button>
          </div>
        </div>
        <InlineError error={save.error} />
      </Card>
    </>
  );
}

function ServiceListRow({
  list,
  selected,
  onSelect,
}: {
  list: BlockList;
  selected: boolean;
  onSelect: () => void;
}) {
  const toggle = useToggleBlockList();
  const remove = useDeleteBlockList();
  const domains = list.websites.filter((rule) => rule.enabled && isDomainRule(rule));
  const supportedApps = list.shared_allowance
    ? 0
    : list.applications.filter((rule) => rule.enabled && isExecutableNameRule(rule)).length;
  const ignored =
    list.applications.length -
    supportedApps +
    list.exceptions.length +
    list.websites.length -
    domains.length;

  return (
    <li>
      <Card
        className={selected ? "border-primary/40 bg-primary/[0.04]" : ""}
        data-testid="service-list-row"
        elevation="raised"
        padding="none"
      >
        <div className="flex items-center gap-3 px-3.5 py-3">
          <Button
            variant="ghost"
            className="min-w-0 flex-1 justify-start px-1 text-left"
            aria-pressed={selected}
            onClick={onSelect}
          >
            <span className="min-w-0 flex-1">
              <span className="block truncate font-medium text-foreground text-sm">
                {list.name}
              </span>
              <span className="mt-1 flex flex-wrap gap-1.5">
                <Badge tone={list.enabled ? "success" : "neutral"}>
                  {list.enabled ? m.lists_badge_enabled() : m.lists_badge_off()}
                </Badge>
                <Badge tone="primary" outlined>
                  {m.service_domains_count({ count: domains.length })}
                </Badge>
                {supportedApps > 0 && (
                  <Badge tone="primary" outlined>
                    {m.count_apps({ count: supportedApps })}
                  </Badge>
                )}
                {ignored > 0 && (
                  <Badge tone="warning" outlined>
                    {m.service_ignored_count({ count: ignored })}
                  </Badge>
                )}
              </span>
            </span>
          </Button>

          <Switch
            checked={list.enabled}
            onCheckedChange={(enabled) => toggle.mutate({ id: list.id, enabled })}
            disabled={toggle.isPending}
            aria-label={m.lists_enable({ name: list.name })}
          />
          <ConfirmButton
            variant="ghost"
            size="icon"
            aria-label={m.lists_delete({ name: list.name })}
            confirmLabel={m.lists_delete_hint()}
            disabled={remove.isPending}
            onConfirm={() => remove.mutate(list.id)}
          >
            <Trash2 />
          </ConfirmButton>
        </div>
        <InlineError error={toggle.error ?? remove.error} />
      </Card>
    </li>
  );
}

function DomainRules({ list }: { list: BlockList }) {
  const [value, setValue] = useState("");
  const add = useAddWebsiteRule();
  const remove = useRemoveWebsiteRule();
  const rules = useMemo(
    () => list.websites.filter((rule) => rule.enabled && isDomainRule(rule)),
    [list.websites],
  );
  const rows = rules.map((rule) => ({ id: rule.id, ...describeWebsite(rule.match_type) }));

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    const domain = value.trim();
    if (!domain) return;
    add.mutate({ listId: list.id, rule: { Domain: domain } }, { onSuccess: () => setValue("") });
  }

  return (
    <>
      <SectionHeading title={m.websites_title()} />
      <Card className="mb-4" padding="md" elevation="raised">
        <form onSubmit={onSubmit} className="flex gap-2">
          <Input
            value={value}
            onChange={(event) => setValue(event.target.value)}
            aria-label={m.websites_value_label()}
            autoCapitalize="none"
            autoCorrect="off"
            spellCheck={false}
          />
          <Button type="submit" icon={<Plus />} disabled={!value.trim() || add.isPending}>
            {m.websites_add()}
          </Button>
        </form>
        <InlineError error={add.error} />
      </Card>

      {rules.length === 0 ? (
        <EmptyState icon={<Globe />} title={m.service_empty_domains()} />
      ) : (
        <RuleTable
          rows={rows}
          noun={m.websites_kind_domain()}
          onRemove={(ruleId) => remove.mutate({ listId: list.id, ruleId })}
        />
      )}
      <InlineError error={remove.error} />
    </>
  );
}

function ServiceHealth({ health }: { health: ReturnType<typeof useBlockingHealth> }) {
  return (
    <section className="mt-7">
      <SectionHeading title={m.service_health_title()} />
      <Card padding="md" elevation="raised">
        <QueryState
          isPending={health.isPending}
          error={health.error}
          onRetry={() => void health.refetch()}
          isRetrying={health.isFetching}
        >
          {health.data && (
            <div className="flex flex-col gap-2 text-sm">
              <p className="text-muted-foreground">
                {m.service_active_lists({ count: health.data.active_lists })}
              </p>
              <p className="text-muted-foreground">
                {health.data.hosts_writable
                  ? m.service_hosts_writable()
                  : m.service_hosts_not_writable()}
              </p>
              {health.data.hosts_sync_error ? (
                <p role="alert" className="break-words text-destructive text-sm">
                  {m.service_hosts_sync_error({ error: health.data.hosts_sync_error })}
                </p>
              ) : (
                <p className="text-faint-foreground text-xs">{m.service_hosts_no_sync_error()}</p>
              )}
            </div>
          )}
        </QueryState>
      </Card>
    </section>
  );
}

function SectionHeading({ title }: { title: string }) {
  return <h2 className="mb-3 mt-7 font-medium text-foreground text-sm">{title}</h2>;
}
