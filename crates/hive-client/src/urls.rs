use super::*;

pub(super) fn session_url(
    endpoint: &Url,
    id: &str,
    suffix: Option<&str>,
) -> Result<Url, ClientError> {
    id_url(endpoint, "/v1/sessions", id, suffix)
}

pub(super) fn id_url(
    endpoint: &Url,
    base: &str,
    id: &str,
    suffix: Option<&str>,
) -> Result<Url, ClientError> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(ClientError::InvalidIdentifier);
    }
    let mut url = endpoint.join(base)?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| ClientError::InvalidOrigin)?;
        segments.pop_if_empty().push(id);
        if let Some(suffix) = suffix {
            segments.push(suffix);
        }
    }
    Ok(url)
}

pub(super) fn query_url(
    endpoint: &Url,
    path: &str,
    after: Option<u64>,
    limit: Option<u32>,
    extra: Option<(&str, &str)>,
) -> Result<Url, ClientError> {
    let mut url = endpoint.join(path)?;
    {
        let mut query = url.query_pairs_mut();
        if let Some(after) = after {
            query.append_pair("after", &after.to_string());
        }
        if let Some(limit) = limit {
            query.append_pair("limit", &limit.to_string());
        }
        if let Some((key, value)) = extra {
            query.append_pair(key, value);
        }
    }
    Ok(url)
}

pub(super) fn append_page_query(url: &mut Url, after: Option<u64>, limit: Option<u32>) {
    let mut query = url.query_pairs_mut();
    if let Some(after) = after {
        query.append_pair("after", &after.to_string());
    }
    if let Some(limit) = limit {
        query.append_pair("limit", &limit.to_string());
    }
}
