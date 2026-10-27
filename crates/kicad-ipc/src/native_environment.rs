//! Bounded environment expansion on one exact native editor and board.
use super::*;

impl<T: KiCadTransport> KiCadClient<T> {
    /// Expand native project/environment paths without running a command or changing settings.
    pub fn expand_environment_paths(
        &mut self,
        filename: &str,
        paths: &[String],
    ) -> Result<Vec<String>, IpcError> {
        let invalid = IpcError::InvalidObservedBoardGraphic;
        if paths.is_empty()
            || paths.len() > 64
            || paths.iter().any(|p| p.len() > 16_384 || p.contains('\0'))
        {
            return Err(invalid("native path expansion exceeds bounds"));
        }
        let token = self.token.clone().ok_or(IpcError::EmptyInstanceToken)?;
        let document = self.require_current_board(&token, filename)?;
        let version: base_commands::GetVersionResponse = self.bound_item_command(
            GET_VERSION_TYPE_URL,
            "type.googleapis.com/kiapi.common.commands.GetVersionResponse",
            base_commands::GetVersion::default(),
        )?;
        if version
            .version
            .as_ref()
            .is_none_or(|v| v.full_version != "10.0.7" || (v.major, v.minor, v.patch) != (10, 0, 7))
        {
            return Err(invalid(
                "native environment expansion requires KiCad 10.0.7",
            ));
        }
        // $(NAME) bypasses the board/title-block text pass, retaining the native
        // environment resolver's project-variable and environment precedence.
        let text = paths
            .iter()
            .map(|p| environment_syntax(p))
            .collect::<Result<Vec<_>, _>>()?;
        let response: project_commands::ExpandTextVariablesResponse = self.bound_item_command(
            "type.googleapis.com/kiapi.common.commands.ExpandTextVariables",
            "type.googleapis.com/kiapi.common.commands.ExpandTextVariablesResponse",
            project_commands::ExpandTextVariables {
                document: Some(document.clone()).into(),
                text,
                expand_env_vars: true,
                ..Default::default()
            },
        )?;
        if response.text.len() != paths.len()
            || response
                .text
                .iter()
                .any(|p| p.len() > 16_384 || p.chars().any(char::is_control))
        {
            return Err(invalid("native path expansion returned an invalid result"));
        }
        if self.require_current_board(&token, filename)? != document {
            return Err(invalid("native path expansion document changed"));
        }
        Ok(response.text)
    }
}

fn environment_syntax(value: &str) -> Result<String, IpcError> {
    let invalid =
        || IpcError::InvalidObservedBoardGraphic("unsupported native environment expression");
    if value.contains('@') || value.contains('\\') {
        return Err(invalid());
    }
    let mut output = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let variable = &rest[start + 2..];
        let end = variable.find('}').ok_or_else(invalid)?;
        let name = &variable[..end];
        if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
            return Err(invalid());
        }
        output.push_str("$(");
        output.push_str(name);
        output.push(')');
        rest = &variable[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn environment_paths_do_not_use_board_text_or_expression_resolution() {
        assert_eq!(
            environment_syntax("${KIPRJMOD}/${CUSTOM}/A.pretty").unwrap(),
            "$(KIPRJMOD)/$(CUSTOM)/A.pretty"
        );
        assert_eq!(
            environment_syntax("$(CUSTOM)/A.pretty").unwrap(),
            "$(CUSTOM)/A.pretty"
        );
        for value in [
            "${TITLE:${OTHER}}",
            "${}",
            "${MISSING",
            "@{1+2}",
            r"\${VAR}",
        ] {
            assert!(environment_syntax(value).is_err());
        }
    }
}
