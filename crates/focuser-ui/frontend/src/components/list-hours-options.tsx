import { ArrowRight, CalendarRange, Lock, Settings2, Unlock } from "lucide-react";
import { type ReactNode, useState } from "react";
import { Link } from "react-router-dom";
import type { BlockList } from "@/bindings";
import { effectiveLock, UnlockDialog } from "@/components/focus-lock-forms";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, ErrorDialog } from "@/components/ui/dialog";
import { Notice } from "@/components/ui/feedback";
import { NumberField } from "@/components/ui/number-field";
import { Switch } from "@/components/ui/switch";
import { Tooltip } from "@/components/ui/tooltip";
import {
  methodFromLock,
  methodReady,
  toLockSetup,
  UnlockMethodPicker,
} from "@/components/unlock-method-picker";
import {
  useBlockingHealth,
  useBrowserStatus,
  useConfigureScheduledProtection,
  useConfigureSharedAllowance,
  useProtectionStatus,
  useRelockScheduledProtection,
  useScheduledProtectionStatus,
  useSharedAllowanceStatus,
} from "@/lib/commands";
import { hasEndingHours, summarizeWeek } from "@/lib/schedule";
import { cn } from "@/lib/utils";
import { m } from "@/paraglide/messages.js";

/** Where the options are shown. The schedule page has the hours right above them. */
type Page = "lists" | "schedule";

/**
 * What a list does with its hours: lock itself while they run, and share one
 * time budget across everything on it.
 *
 * Each option is one row with a switch. Setting one up, unlocking, and anything
 * that goes wrong happen in a pop-up, so the list itself stays short. Both need
 * hours that end; a list without them gets a way to set them instead of an
 * error after the click.
 */
export function ListHoursOptions({ list, page }: { list: BlockList; page: Page }) {
  const protection = useProtectionStatus();
  const hours = hasEndingHours(list.schedule);
  // Either kind of lock. The command core refuses every change below while one
  // is running, so the controls say so first.
  const locked =
    protection.data?.some((p) => p.block_list_id === list.id && p.prevent_modification) ?? false;

  return (
    <div className={cn("divide-y divide-border", page === "lists" && "border-border border-t")}>
      <ScheduledLockRow list={list} page={page} hours={hours} locked={locked} />
      <SharedAllowanceRow list={list} hours={hours} locked={locked} />
      <HoursLine list={list} page={page} hours={hours} locked={locked} />
    </div>
  );
}

const ROW = "flex min-h-12 flex-wrap items-center gap-x-3 gap-y-2 px-4 py-2";

interface RowProps {
  list: BlockList;
  hours: boolean;
  locked: boolean;
}

function ScheduledLockRow({ list, page, hours, locked }: RowProps & { page: Page }) {
  const status = useScheduledProtectionStatus();
  const configure = useConfigureScheduledProtection();
  const relock = useRelockScheduledProtection();
  const [settingUp, setSettingUp] = useState(false);
  const [unlocking, setUnlocking] = useState(false);

  const state = status.data?.find((s) => s.block_list_id === list.id)?.state;
  const on = !!list.scheduled_protection;
  const busy = configure.isPending || relock.isPending;
  const lock = list.scheduled_protection?.lock;
  const method = !lock
    ? m.schedule_lock_none_summary()
    : lock.RandomText
      ? m.schedule_lock_random_summary({ count: lock.RandomText.length })
      : m.lists_lock_kind_password();

  return (
    <div className={ROW}>
      <Switch
        size="sm"
        checked={on}
        aria-label={`${m.schedule_protection_label()}: ${list.name}`}
        // Off can always be reached while the list is not locked. On needs
        // hours for the lock to follow.
        disabled={!state || locked || busy || (!on && !hours)}
        onCheckedChange={(next) =>
          next
            ? setSettingUp(true)
            : configure.mutate({ listId: list.id, enabled: false, lock: null })
        }
      />
      <span className={on || hours ? "text-foreground text-sm" : "text-muted-foreground text-sm"}>
        {m.schedule_protection_label()}
      </span>

      {on && state && (
        <>
          {state === "locked" && page === "schedule" && (
            <Badge tone="warning" icon={<Lock aria-hidden />} outlined>
              {m.schedule_lock_locked()}
            </Badge>
          )}
          {state === "unlocked_for_editing" && (
            <Badge tone="success" icon={<Unlock aria-hidden />} outlined>
              {m.schedule_lock_editing()}
            </Badge>
          )}
          <span className="text-faint-foreground text-xs">{method}</span>

          <span className="ml-auto flex items-center gap-2">
            {state === "inactive" && (
              <Tooltip content={m.schedule_lock_next()}>
                <span className="text-muted-foreground text-xs">{m.schedule_lock_waiting()}</span>
              </Tooltip>
            )}
            {state !== "locked" && !locked && (
              <Tooltip content={m.schedule_lock_change()}>
                <Button
                  variant="ghost"
                  size="icon"
                  className="size-8"
                  aria-label={m.schedule_lock_change()}
                  disabled={busy}
                  onClick={() => setSettingUp(true)}
                >
                  <Settings2 />
                </Button>
              </Tooltip>
            )}
            {state === "locked" && effectiveLock(list) && (
              <Button
                variant="outline"
                size="sm"
                icon={<Unlock />}
                onClick={() => setUnlocking(true)}
              >
                {m.schedule_lock_unlock()}
              </Button>
            )}
            {state === "unlocked_for_editing" && (
              <Button
                variant="outline"
                size="sm"
                icon={<Lock />}
                disabled={busy}
                onClick={() => relock.mutate(list.id)}
              >
                {m.schedule_lock_again()}
              </Button>
            )}
          </span>
        </>
      )}

      <ScheduledLockDialog list={list} open={settingUp} onClose={() => setSettingUp(false)} />
      <UnlockDialog list={list} open={unlocking} onClose={() => setUnlocking(false)} />
      <ErrorDialog
        error={configure.error ?? relock.error}
        onClose={() => {
          configure.reset();
          relock.reset();
        }}
      />
    </div>
  );
}

/** Turning the lock on, or changing how it can be unlocked. */
function ScheduledLockDialog({
  list,
  open,
  onClose,
}: {
  list: BlockList;
  open: boolean;
  onClose: () => void;
}) {
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={m.schedule_lock_dialog_title({ name: list.name })}
      icon={<Lock aria-hidden className="text-warning" />}
    >
      <ScheduledLockForm list={list} onDone={onClose} />
    </Dialog>
  );
}

function ScheduledLockForm({ list, onDone }: { list: BlockList; onDone: () => void }) {
  const configure = useConfigureScheduledProtection();
  const saved = list.scheduled_protection;
  // A new lock starts on the random text: it can be undone, and it still costs
  // something to undo. An existing one starts on what it already has.
  const [method, setMethod] = useState(() => methodFromLock(saved ? saved.lock : undefined));

  return (
    <>
      <p className="mt-2 text-muted-foreground text-sm leading-relaxed">
        {m.schedule_lock_dialog_body()}
      </p>

      <div className="mt-4">
        <UnlockMethodPicker value={method} onChange={setMethod} disabled={configure.isPending} />
      </div>

      <Notice error={configure.error} />

      <div className="mt-5 flex items-center justify-end gap-2">
        <Button variant="ghost" size="sm" onClick={onDone}>
          {m.lists_cancel()}
        </Button>
        <Button
          size="sm"
          disabled={configure.isPending || !methodReady(method)}
          onClick={() =>
            configure.mutate(
              { listId: list.id, enabled: true, lock: toLockSetup(method) },
              { onSuccess: onDone },
            )
          }
        >
          {saved ? m.common_save() : m.schedule_lock_turn_on()}
        </Button>
      </div>
    </>
  );
}

function SharedAllowanceRow({ list, hours, locked }: RowProps) {
  const configure = useConfigureSharedAllowance();
  const statuses = useSharedAllowanceStatus();
  const health = useBlockingHealth();
  const browsers = useBrowserStatus();

  const config = list.shared_allowance;
  const status = statuses.data?.find((s) => s.block_list_id === list.id);
  const disabled = locked || configure.isPending;
  const save = (minutes: number | null) => configure.mutate({ listId: list.id, minutes });

  // Why a budget that is switched on may still open nothing.
  const warnings: ReactNode[] = [];
  if (config) {
    const hasSites = list.websites.some((r) => r.enabled);
    const hasApps = list.applications.some((r) => r.enabled);
    if (hasSites && browsers.data && !browsers.data.some((b) => b.extension_connected)) {
      warnings.push(m.allowances_extension_needed_title());
    }
    if (hasApps && health.data?.app_usage_measurable === false) {
      warnings.push(m.allowances_wayland_body());
    }
  }

  return (
    <div className={ROW}>
      <Switch
        size="sm"
        checked={!!config}
        aria-label={`${m.shared_allowance_label()}: ${list.name}`}
        disabled={disabled || (!config && !hours)}
        onCheckedChange={(next) => save(next ? 30 : null)}
      />
      <Tooltip content={m.shared_allowance_hint()}>
        <span
          className={config || hours ? "text-foreground text-sm" : "text-muted-foreground text-sm"}
        >
          {m.shared_allowance_label()}
        </span>
      </Tooltip>

      {config && (
        <>
          <NumberField
            // The field keeps the number it sent until the list echoes it back.
            // A refused one never comes back, so start the field again.
            key={configure.isError ? "refused" : "saved"}
            value={config.minutes}
            min={1}
            max={1440}
            disabled={disabled}
            onCommit={save}
            aria-label={m.shared_allowance_minutes()}
          />
          <span className="text-faint-foreground text-xs">{m.shared_allowance_period()}</span>

          {status?.active && (
            <Badge
              className="ml-auto"
              tone={status.remaining_secs === 0 ? "warning" : "neutral"}
              outlined
            >
              {status.remaining_secs === 0
                ? m.shared_allowance_used_up()
                : m.shared_allowance_left({ remaining: Math.ceil(status.remaining_secs / 60) })}
            </Badge>
          )}
        </>
      )}

      {warnings.map((warning) => (
        <p key={String(warning)} className="basis-full text-warning text-xs">
          {warning}
        </p>
      ))}

      <ErrorDialog error={configure.error} onClose={() => configure.reset()} />
    </div>
  );
}

function HoursLine({ list, page, hours, locked }: RowProps & { page: Page }) {
  // On the schedule page the hours are the grid above. All that is left to say
  // there is why the two switches are greyed out.
  if (page === "schedule") {
    return hours ? null : (
      <p className={`${ROW} text-muted-foreground text-xs`}>{m.hours_none_here()}</p>
    );
  }

  return (
    <div className={`${ROW} text-xs`}>
      <CalendarRange aria-hidden className="size-4 shrink-0 text-faint-foreground" />
      {hours ? (
        <>
          <span className="min-w-0 flex-1 truncate text-muted-foreground">
            {summarizeWeek(list.schedule?.time_slots ?? [])}
          </span>
          {!locked && (
            <Button asChild variant="link" size="sm" className="h-auto px-0">
              <Link to={`/schedule?list=${list.id}`}>{m.hours_edit()}</Link>
            </Button>
          )}
        </>
      ) : (
        <>
          <span className="min-w-0 flex-1 text-muted-foreground">{m.hours_none()}</span>
          {!locked && (
            <Button asChild variant="outline" size="sm">
              <Link to={`/schedule?list=${list.id}&set=hours`}>
                {m.hours_set()}
                <ArrowRight aria-hidden />
              </Link>
            </Button>
          )}
        </>
      )}
    </div>
  );
}
