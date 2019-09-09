use nom::types::CompleteByteSlice;
use ::parser::{le_u32, parse_dict, Dict};
use std::collections::HashMap;

/*
(1) Transform Node Chunk : "nTRN"

int32	: node id
DICT	: node attributes
	  (_name : string)
	  (_hidden : 0/1)
int32 	: child node id
int32 	: reserved id (must be -1)
int32	: layer id
int32	: num of frames (must be 1)

// for each frame
{
DICT	: frame attributes
	  (_r : int8) ROTATION, see (c)
	  (_t : int32x3) translation
}xN

=================================
(2) Group Node Chunk : "nGRP" 

int32	: node id
DICT	: node attributes
int32 	: num of children nodes

// for each child
{
int32	: child node id
}xN

=================================
(3) Shape Node Chunk : "nSHP" 

int32	: node id
DICT	: node attributes
int32 	: num of models (must be 1)

// for each model
{
int32	: model id
DICT	: model attributes : reserved
}xN
*/

#[derive(Debug, PartialEq)]
pub struct Node {
    pub id: u32,
    pub kind: NodeKind,
}

#[derive(Debug, PartialEq)]
pub enum NodeKind {
    Group {
        children_ids: Vec<u32>,
    },
    Transform {
        child_id: u32,
        transform: Transform,
    },
    Shape {  
        model_id: u32,
    }
}

/// TODO doc
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    /// Translation
    pub t: [i32; 3],
    /// Row-major rotation matrix
    pub r: [[i8; 3]; 3],
}
impl Transform {
    fn default() -> Self {
        Self {
            t: [0, 0, 0],
            r: [[1, 0, 0], [0, 1, 0], [0, 0, 1]],
        }
    }
    fn apply(self, other: Self) -> Self {
        let dot_i32 = |v1: [i32; 3], v2: [i32; 3] | v1[0] * v2[0] + v1[1] * v2[1] + v1[2] * v2[2];
        let dot_i8 = |v1: [i8; 3], v2: [i8; 3] | v1[0] * v2[0] + v1[1] * v2[1] + v1[2] * v2[2];
        let add = |v1: [i32; 3], v2: [i32; 3] | [v1[0] + v2[0], v1[1] + v2[1], v1[2] + v2[2]];
        let row_i32 = |m: [[i8; 3]; 3], r: usize| [m[r][0] as i32, m[r][1] as i32, m[r][2] as i32];
        let col_i8 = |m: [[i8; 3]; 3], c| [m[0][c], m[1][c], m[2][c]];
        let mul_mv_i32 = |m: [[i8; 3]; 3], v: [i32; 3]| [
            dot_i32(row_i32(m, 0), v),
            dot_i32(row_i32(m, 1), v),
            dot_i32(row_i32(m, 2), v),
        ];
        let mul_vm_i8 = |v: [i8; 3], m: [[i8; 3]; 3]| [
            dot_i8(v, col_i8(m, 0)),
            dot_i8(v, col_i8(m, 1)),
            dot_i8(v, col_i8(m, 2)),
        ];

        let t = add(mul_mv_i32(other.r, self.t), other.t);
        let r = [
            mul_vm_i8(other.r[0], self.r),
            mul_vm_i8(other.r[1], self.r),
            mul_vm_i8(other.r[2], self.r),
        ];

        Self {
            t, 
            r,
        }
    }
    fn from_dict(dict: Dict) -> Self {
        let t = dict.get("_t").and_then(|s| {
            let values = s.split(' ').map(str::parse::<i32>).filter_map(Result::ok).collect::<Vec<_>>();
            if values.len() == 3 {
                Some([values[0], values[1], values[2]])
            } else {
                debug!("Unknown translation format: {}", s);
                None
            }
        }).unwrap_or([0; 3]);

        // 0-1 : 1 : index of the non-zero entry in the first row
        // 2-3 : 2 : index of the non-zero entry in the second row
        // 4   : 0 : the sign in the first row (0 : positive; 1 : negative)
        // 5   : 1 : the sign in the second row (0 : positive; 1 : negative)
        // 6   : 1 : the sign in the third row (0 : positive; 1 : negative)
        let r = dict.get("_r").and_then(|s| {
            s.parse::<u8>().ok().and_then(|n| {
                let signs = [
                    if n >> 4 & 1 == 0 { 1 } else { -1 },
                    if n >> 5 & 1 == 0 { 1 } else { -1 },
                    if n >> 6 & 1 == 0 { 1 } else { -1 },
                ];
                let rows = [
                    [1, 0, 0],
                    [0, 1, 0],
                    [0, 0, 1],
                ];
                if n & 3 != 3 && n >> 2 != 3 {
                    let r1 = rows[(n & 3) as usize];
                    let r2 = rows[(n >> 2 & 3) as usize];
                    let r3 = rows[(!(n | (n >> 2)) & 3) as usize];
                    Some([
                        [r1[0]*signs[0], r1[1]*signs[0], r1[2]*signs[0]],
                        [r2[0]*signs[1], r2[1]*signs[1], r2[2]*signs[1]],
                        [r3[0]*signs[2], r3[1]*signs[2], r3[2]*signs[2]],
                    ])
                } else {
                    debug!("Unknown rotation format: {}", s);
                    None
                }
            })
        }).unwrap_or([[1, 0, 0], [0, 1, 0], [0, 0, 1]]);

        Self {
            t, r
        }
    }
}

pub struct SceneGraph(HashMap<u32, NodeKind>);
impl SceneGraph {
    pub fn new() -> Self {
        Self(HashMap::new())
    }
    pub fn add_node(&mut self, node: Node) {
        self.0.insert(node.id, node.kind);
    }
    pub fn collapse_to_vec(self) -> Vec<(Transform, usize)> {
        // Assume that we have no cycles
        // Assume root node id is 0 and it is a Transform node
        if let Some(NodeKind::Transform{ child_id, transform }) = self.0.get(&0) {
            self.collapse_transform(*child_id, vec![*transform])
                .iter()
                .map(|(transforms, id)| (
                    transforms
                        .iter()
                        .fold(
                            Transform::default(),
                            |transform, next| transform.apply(*next),
                        ),
                    *id,
                )).collect::<Vec<_>>()
        } else {
            debug!("Unknown scene graph format: node 0 is not a Transform node");
            vec![]
        }
    }
    fn collapse_transform(&self, child: u32, transforms: Vec<Transform>) -> Vec<(Vec<Transform>, usize)> {
        let mut collapsed = Vec::new();

        if let Some(node) = self.0.get(&child) {
            match node {
                NodeKind::Group{ children_ids } => {
                    for id in children_ids {
                        match self.0.get(id) {
                            Some(NodeKind::Transform{ child_id, transform }) => {
                                let mut new_transforms = vec![*transform];
                                new_transforms.extend_from_slice(&transforms);
                                collapsed.append(&mut self.collapse_transform(*child_id, new_transforms));
                            }
                            Some(_) => {
                                debug!("Unknown scene graph format: non-Transform node found as Group node child");
                            }
                            None => {
                                debug!("Scene graph contains an id for a node which doesn't exist (id: {})", id);
                            }
                        }
                    }
                }
                NodeKind::Shape { model_id } => collapsed.push((transforms, *model_id as usize)),
                NodeKind::Transform { .. } =>  debug!("Unknown scene graph format: Transform node found as Transform node child"),
            }
        } else {
            debug!("Scene graph contains an id for a node which doesn't exist (id: {})", child);
        }

        collapsed
    }
}


named!(pub parse_group_node <CompleteByteSlice, Node>, do_parse!(
    id: le_u32 >>
    _attributes: parse_dict >>
    num_children: le_u32 >>
    children_ids: many_m_n!(num_children as usize, num_children as usize, le_u32) >>
    (Node { id, kind: NodeKind::Group { children_ids } })
));

named!(pub parse_transform_node <CompleteByteSlice, Node>, do_parse!(
    id: le_u32 >>
    _attributes: parse_dict >>
    child_id: le_u32 >>
    _reserved_id: le_u32 >> // must be -1
    _layer_id: le_u32 >>
    _num_frames: le_u32 >> // must be 1
    transform_dict: parse_dict >>
    (Node { id, kind: NodeKind::Transform { child_id, transform: Transform::from_dict(transform_dict) } })
));

named!(pub parse_shape_node <CompleteByteSlice, Node>, do_parse!(
    id: le_u32 >>
    _attributes: parse_dict >>
    _num_models: le_u32 >> // must be 1
    model_id: parse_model_entry >>
    (Node { id, kind: NodeKind::Shape { model_id } })
));

named!(parse_model_entry <CompleteByteSlice, u32>, do_parse!(
    id: le_u32 >>
    _attributes: parse_dict >>
    (id)
));