type DeviceStatusBadgeProps = {
  online: boolean;
};

export function DeviceStatusBadge({ online }: DeviceStatusBadgeProps) {
  return (
    <span className="status-badge" data-online={online ? "true" : "false"}>
      {online ? "Online" : "Offline"}
    </span>
  );
}
