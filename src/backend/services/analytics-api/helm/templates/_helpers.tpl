{{- define "insight-analytics-api.fullname" -}}
{{ .Release.Name }}-analytics-api
{{- end }}

{{- define "insight-analytics-api.labels" -}}
app.kubernetes.io/name: analytics-api
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "insight-analytics-api.selectorLabels" -}}
app.kubernetes.io/name: analytics-api
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}
