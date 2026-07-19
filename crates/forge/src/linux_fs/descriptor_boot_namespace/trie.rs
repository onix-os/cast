use super::{
    budget::Operation, error::BootNamespaceAssessmentError, model::BootNamespaceRequest,
    observer::BootNamespaceObserver,
};

pub(super) struct RequestTrie<'a> {
    nodes: Vec<RequestTrieNode<'a>>,
}

pub(super) struct RequestTrieNode<'a> {
    children: Vec<RequestTrieEdge<'a>>,
    leaf_request: Option<usize>,
    context_request: usize,
    depth: usize,
}

pub(super) struct RequestTrieEdge<'a> {
    component: &'a [u8],
    child: usize,
    request_index: usize,
    component_index: usize,
}

impl<'a> RequestTrie<'a> {
    pub(super) fn build<Observer: BootNamespaceObserver>(
        requests: &'a [BootNamespaceRequest<'a>],
        operation: &mut Operation<'_, Observer>,
    ) -> Result<Self, BootNamespaceAssessmentError> {
        let limits = operation.limits();
        if requests.len() > limits.max_requests {
            return Err(BootNamespaceAssessmentError::RequestLimitExceeded {
                limit: limits.max_requests,
                found: requests.len(),
            });
        }

        let mut trie = Self { nodes: Vec::new() };
        operation.reserve(&mut trie.nodes, 1, "allocating the request trie root")?;
        trie.nodes.push(RequestTrieNode {
            children: Vec::new(),
            leaf_request: None,
            context_request: 0,
            depth: 0,
        });

        for (request_index, request) in requests.iter().copied().enumerate() {
            operation.charge_work(1, "validating one canonical namespace request")?;
            let path = request.relative_path().as_bytes();
            validate_path(request_index, path, operation)?;

            let mut node_index = 0usize;
            for (component_index, component) in path.split(|byte| *byte == b'/').enumerate() {
                if let Some(first_request) = trie.nodes[node_index].leaf_request {
                    return Err(BootNamespaceAssessmentError::RequestHierarchyCollision {
                        first_request,
                        second_request: request_index,
                    });
                }

                let mut exact_child = None;
                for edge in &trie.nodes[node_index].children {
                    operation.charge_work(1, "checking request collision-domain trie edges")?;
                    if edge.component.eq_ignore_ascii_case(component) {
                        if edge.component != component {
                            return Err(BootNamespaceAssessmentError::RequestCollision {
                                first_request: edge.request_index,
                                second_request: request_index,
                            });
                        }
                        exact_child = Some(edge.child);
                        break;
                    }
                }

                node_index = if let Some(child) = exact_child {
                    child
                } else {
                    let child = trie.nodes.len();
                    operation.reserve(&mut trie.nodes, 1, "allocating one request trie node")?;
                    trie.nodes.push(RequestTrieNode {
                        children: Vec::new(),
                        leaf_request: None,
                        context_request: request_index,
                        depth: component_index + 1,
                    });
                    operation.reserve(
                        &mut trie.nodes[node_index].children,
                        1,
                        "allocating one request trie edge",
                    )?;
                    trie.nodes[node_index].children.push(RequestTrieEdge {
                        component,
                        child,
                        request_index,
                        component_index,
                    });
                    child
                };
            }

            if let Some(first_request) = trie.nodes[node_index].leaf_request {
                return Err(BootNamespaceAssessmentError::RequestCollision {
                    first_request,
                    second_request: request_index,
                });
            }
            if let Some(first_edge) = trie.nodes[node_index].children.first() {
                return Err(BootNamespaceAssessmentError::RequestHierarchyCollision {
                    first_request: first_edge.request_index,
                    second_request: request_index,
                });
            }
            trie.nodes[node_index].leaf_request = Some(request_index);
        }
        operation.checkpoint()?;
        Ok(trie)
    }

    pub(super) fn root(&self) -> &RequestTrieNode<'a> {
        &self.nodes[0]
    }

    pub(super) fn node(&self, index: usize) -> &RequestTrieNode<'a> {
        &self.nodes[index]
    }
}

impl<'a> RequestTrieNode<'a> {
    pub(super) fn children(&self) -> &[RequestTrieEdge<'a>] {
        &self.children
    }

    pub(super) const fn leaf_request(&self) -> Option<usize> {
        self.leaf_request
    }

    pub(super) const fn context_request(&self) -> usize {
        self.context_request
    }

    pub(super) const fn depth(&self) -> usize {
        self.depth
    }
}

impl<'a> RequestTrieEdge<'a> {
    pub(super) const fn component(&self) -> &'a [u8] {
        self.component
    }

    pub(super) const fn child(&self) -> usize {
        self.child
    }

    pub(super) const fn request_index(&self) -> usize {
        self.request_index
    }

    pub(super) const fn component_index(&self) -> usize {
        self.component_index
    }
}

fn validate_path<Observer: BootNamespaceObserver>(
    request_index: usize,
    path: &[u8],
    operation: &mut Operation<'_, Observer>,
) -> Result<(), BootNamespaceAssessmentError> {
    let limits = operation.limits();
    if path.len() > limits.max_path_bytes {
        return Err(BootNamespaceAssessmentError::RequestPathLimitExceeded {
            request_index,
            limit: limits.max_path_bytes,
            found: path.len(),
        });
    }
    operation.charge_request_path(path.len())?;
    if path.is_empty()
        || !path.is_ascii()
        || path.starts_with(b"/")
        || path.ends_with(b"/")
        || path.contains(&0)
        || path.contains(&b'\\')
    {
        return Err(BootNamespaceAssessmentError::InvalidRequestPath { request_index });
    }

    let mut count = 0usize;
    for (component_index, component) in path.split(|byte| *byte == b'/').enumerate() {
        operation.charge_work(1, "validating one requested path component")?;
        count = count
            .checked_add(1)
            .ok_or(BootNamespaceAssessmentError::RequestComponentLimitExceeded {
                request_index,
                limit: limits.max_components_per_request,
                found: usize::MAX,
            })?;
        if component.is_empty() || component == b"." || component == b".." {
            return Err(BootNamespaceAssessmentError::InvalidRequestPath { request_index });
        }
        if component.len() > limits.max_component_bytes {
            return Err(BootNamespaceAssessmentError::RequestComponentNameLimitExceeded {
                request_index,
                component_index,
                limit: limits.max_component_bytes,
                found: component.len(),
            });
        }
    }
    if count > limits.max_components_per_request {
        return Err(BootNamespaceAssessmentError::RequestComponentLimitExceeded {
            request_index,
            limit: limits.max_components_per_request,
            found: count,
        });
    }
    Ok(())
}
