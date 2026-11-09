import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import { SettingRow, SettingsSection } from "@/components/setting-row";
import { Button } from "@/components/ui/button";
import { InlineError } from "@/components/ui/feedback";
import { hostPlatform, isTauri } from "@/lib/native";
import { m } from "@/paraglide/messages.js";

const STATUS_KEY = ["macos-service-status"] as const;

type ServiceStatus =
  | "enabled"
  | "requires_approval"
  | "not_registered"
  | "not_found"
  | "unsupported";

export function MacosServiceSetup() {
  const queryClient = useQueryClient();
  const [requestError, setRequestError] = useState<Error | null>(null);
  const visible = isTauri() && hostPlatform() === "macos";
  const status = useQuery({
    queryKey: STATUS_KEY,
    queryFn: () => invoke<ServiceStatus>("macos_service_status"),
    enabled: visible,
    refetchInterval: 5_000,
    retry: false,
  });
  const register = useMutation({
    mutationFn: () => invoke<void>("register_macos_service"),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: STATUS_KEY }),
    onError: setRequestError,
  });
  const openSettings = useMutation({
    mutationFn: () => invoke<void>("open_macos_service_settings"),
    onError: setRequestError,
  });

  if (!visible) return null;

  const stateLabel = (() => {
    switch (status.data) {
      case "enabled":
        return m.macos_service_enabled();
      case "requires_approval":
        return m.macos_service_requires_approval();
      case "not_found":
        return m.macos_service_not_found();
      case "unsupported":
        return m.macos_service_unsupported();
      default:
        return m.macos_service_not_registered();
    }
  })();
  const needsApproval = status.data === "requires_approval" || status.data === "enabled";

  return (
    <SettingsSection
      title={m.macos_service_section_title()}
      description={m.macos_service_section_description()}
    >
      <SettingRow
        label={m.macos_service_status_label()}
        description={stateLabel}
        control={
          <div className="flex flex-wrap justify-end gap-2">
            {status.data !== "enabled" && status.data !== "requires_approval" && (
              <Button
                variant="outline"
                size="sm"
                onClick={() => {
                  setRequestError(null);
                  register.mutate();
                }}
                disabled={
                  status.isPending ||
                  status.isError ||
                  register.isPending ||
                  status.data === "not_found" ||
                  status.data === "unsupported"
                }
              >
                {m.macos_service_register()}
              </Button>
            )}
            {needsApproval && (
              <Button
                variant="outline"
                size="sm"
                onClick={() => {
                  setRequestError(null);
                  openSettings.mutate();
                }}
                disabled={openSettings.isPending}
              >
                {m.macos_service_open_settings()}
              </Button>
            )}
            <Button
              variant="ghost"
              size="sm"
              onClick={() => void status.refetch()}
              disabled={status.isFetching}
            >
              {m.macos_service_refresh()}
            </Button>
          </div>
        }
      />
      <InlineError error={status.error ?? requestError} />
    </SettingsSection>
  );
}
