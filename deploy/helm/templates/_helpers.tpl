{{/*
Common labels — applied to every resource the chart creates.
*/}}
{{- define "propfirm.labels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end -}}

{{/*
Selector labels — applied to every resource so k8s can match pods to
the deployments that own them.
*/}}
{{- define "propfirm.selectorLabels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/*
The image reference (repository + tag, falling back to chart appVersion
when the user didn't override).
*/}}
{{- define "propfirm.image" -}}
{{- $tag := .Values.image.tag | default .Chart.AppVersion -}}
{{ .Values.image.repository }}:{{ $tag }}
{{- end -}}

{{/*
The full config map name.
*/}}
{{- define "propfirm.configMapName" -}}
{{- printf "%s-config" .Release.Name -}}
{{- end -}}

{{/*
The TLS secret names.
*/}}
{{- define "propfirm.tlsSecretName" -}}
{{- default (printf "%s-tls" .Release.Name) .Values.server.tls.certSecret -}}
{{- end -}}

{{- define "propfirm.clientCaSecretName" -}}
{{- default (printf "%s-client-ca" .Release.Name) .Values.server.tls.clientCaSecret -}}
{{- end -}}

{{/*
Postgres credentials secret name (used when postgres.deploy=false).
*/}}
{{- define "propfirm.postgresSecretName" -}}
{{- printf "%s-postgres" .Release.Name -}}
{{- end -}}

{{/*
Redis secret name (used when redis.deploy=false).
*/}}
{{- define "propfirm.redisSecretName" -}}
{{- printf "%s-redis" .Release.Name -}}
{{- end -}}
