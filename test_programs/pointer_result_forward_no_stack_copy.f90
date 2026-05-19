! CHECK: ok
! IR_CHECK: call @afs_modproc_parser_like_parse_simple_cmd
! IR_NOT: call @memcpy(
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module tree_like
  implicit none

  type :: command_node_t
    integer :: node_type = 0
    character(len=4096) :: payload = ''
    type(command_node_t), pointer :: child => null()
  end type
contains
  function create_simple_command() result(node)
    type(command_node_t), pointer :: node
    allocate(node)
    node%node_type = 1
    node%payload(1:2) = 'ok'
  end function

  subroutine destroy_command_node(node)
    type(command_node_t), pointer, intent(inout) :: node
    if (.not. associated(node)) error stop 1
    deallocate(node)
    nullify(node)
  end subroutine
end module

module parser_like
  use tree_like
  implicit none

  type :: parser_state_t
    integer :: token = 0
  end type
contains
  recursive function parse_command_node(state) result(node)
    type(parser_state_t), intent(inout) :: state
    type(command_node_t), pointer :: node
    if (state%token == 1) then
      nullify(node)
    else
      node => parse_simple_cmd(state)
    end if
  end function

  function parse_simple_cmd(state) result(node)
    type(parser_state_t), intent(inout) :: state
    type(command_node_t), pointer :: node, func_body
    nullify(func_body)
    state%token = state%token + 1
    node => create_simple_command()
  end function
end module

program pointer_result_forward_no_stack_copy
  use parser_like
  use tree_like
  implicit none

  type(parser_state_t) :: state
  type(command_node_t), pointer :: root

  root => parse_command_node(state)
  if (.not. associated(root)) error stop 2
  if (root%node_type /= 1) error stop 3
  if (root%payload(1:2) /= 'ok') error stop 4
  call destroy_command_node(root)
  if (associated(root)) error stop 5
  print *, 'ok'
end program
