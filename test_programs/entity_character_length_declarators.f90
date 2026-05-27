! CHECK: ok
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module entity_character_length_declarators_mod
  implicit none

  type :: token_info
    character :: tag*3
    character :: word*5 = 'abc'
  end type token_info

contains
  integer function mini_ilaenv_name_probe(name) result(nb)
    character(len=*), intent(in) :: name
    character :: c1*1, c2*2, c3*3, c4*2, subnam*16

    nb = 1
    subnam = name
    c1 = subnam(1:1)
    c2 = subnam(2:3)
    c3 = subnam(4:6)
    c4 = c3(2:3)
    if ((c1 == 'S' .or. c1 == 'D') .and. c2 == 'GE') then
      if (c3 == 'QRF' .or. c4 == 'RF') nb = 32
    end if
  end function mini_ilaenv_name_probe
end module entity_character_length_declarators_mod

program entity_character_length_declarators
  use entity_character_length_declarators_mod
  implicit none

  character :: c1*1, c2*2, c3*3, subnam*16
  type(token_info) :: token

  subnam = 'SGEQRF'
  c1 = subnam(1:1)
  c2 = subnam(2:3)
  c3 = subnam(4:6)

  if (len(c1) /= 1) error stop 1
  if (len(c2) /= 2) error stop 2
  if (len(c3) /= 3) error stop 3
  if (len(subnam) /= 16) error stop 4
  if (c1 /= 'S') error stop 5
  if (c2 /= 'GE') error stop 6
  if (c3 /= 'QRF') error stop 7
  if (mini_ilaenv_name_probe('SGEQRF') /= 32) error stop 8

  token%tag = c3
  if (len(token%tag) /= 3) error stop 9
  if (len(token%word) /= 5) error stop 10
  if (token%tag /= 'QRF') error stop 11
  if (token%word /= 'abc  ') error stop 12

  print *, 'ok'
end program entity_character_length_declarators
